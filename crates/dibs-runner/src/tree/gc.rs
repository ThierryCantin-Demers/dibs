//! `dibs --gc`: what fills a machine is its own worktrees, build caches and job logs, swept by age.
//! It runs as a shared job, so the deleting queues behind a measurement rather than competing
//! with one, and the log carries what it reclaimed.

use crate::{
    clock::Moment,
    itself::Itself,
    machine::Site,
    platform::{Host, Platform as _},
    settings::{Settings, home},
    tree::{
        clocks::{Clocks, Contents as _, Dates as _, Fate, Removal},
        runners::Runners,
        sweep::{Section, Sweep, Swept, Verdict},
    },
};
use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    fmt, fs,
    io::{self, Write as _},
    iter,
    num::NonZero,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    panic,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

/// Rows listed under a heading before the rest are counted; what is past its clock always is.
const LISTED: usize = 20;
/// Directories under scratch that dibs makes; anything else is somebody's and never removed.
const DIBS_OWN: [&str; 7] = ["ws", "target", "jobs", "tmp", "out", "runner", "run"];

/// `dibs --gc`'s listing of one sweep, a section at a time as the sweep goes: what each holds,
/// biggest first, and what went or would.
pub struct SweepReport {
    scratch: PathBuf,
    /// Where the runners live, and the clones a removed worktree was added from.
    home: PathBuf,
    host: String,
    clocks: Clocks,
    /// Only say what would go.
    dry: bool,
}

/// KiB measured, reclaimed and past their clocks.
#[derive(Default)]
struct Totals {
    total: u64,
    freed: u64,
    would: u64,
}

/// Lines under a heading, biggest first: the question behind the command is where the disk went.
#[derive(Default)]
struct Rows {
    rows: Vec<Row>,
}

struct Row {
    kib: u64,
    past: bool,
    line: String,
}

/// What removing each build cache alone would free, and what they share, where blocks are shared.
struct Sharing {
    own: HashMap<PathBuf, u64>,
    shared: u64,
}

struct Measured {
    sizes: Sizes,
    sharing: Option<Sharing>,
}

/// One path's sizes, read in a single walk of it.
#[derive(Default)]
struct Walked {
    /// 512-byte blocks, a file linked twice under the path counted once.
    blocks: u64,
    /// Its files linked more than once, by device and inode, with their blocks: `du` counts each
    /// in the first path given that holds it.
    linked: HashMap<(u64, u64), u64>,
    /// KiB in extents no other file shares.
    alone: u64,
    /// KiB in shared extents, by where each starts on the disk, so two caches' copies count once.
    shared: HashMap<u64, u64>,
}

/// What `--gc` was asked, as the request carries it in the command's place: `<days|default>
/// <0|1>`.
pub struct Asked {
    /// None leaves the machine's own clocks.
    pub days: Option<u64>,
    pub dry: bool,
}

impl Asked {
    pub fn parse(words: &str) -> Asked {
        let mut words = words.split_whitespace();
        Asked {
            days: words.next().and_then(|d| d.parse().ok()),
            dry: words.next() == Some("1"),
        }
    }

    /// The call as the records and the log name it.
    pub fn named(&self) -> String {
        let mut named = "dibs --gc".to_string();
        if let Some(days) = self.days {
            named.push_str(&format!(" --days {days}"));
        }
        if self.dry {
            named.push_str(" --dry-run");
        }
        named
    }

    /// The job that sweeps: this runner again.
    pub fn command(&self) -> String {
        let mut words = Itself::words();
        words.push("gc".to_string());
        words.push(self.days.map_or("default".to_string(), |d| d.to_string()));
        words.push(u8::from(self.dry).to_string());
        words
            .iter()
            .map(|w| format!("'{}'", w.replace('\'', r"'\''")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The report, with the machine's own clocks unless days were given.
    pub fn report(&self) -> SweepReport {
        let settings = Settings::load();
        let machine = Site::set_up().ok();
        let home = home();
        SweepReport {
            scratch: machine
                .as_ref()
                .map_or_else(|| home.join(".cache/dibs"), |m| m.scratch.clone()),
            home,
            host: crate::machine::short_hostname(None),
            clocks: match self.days {
                Some(days) => Clocks {
                    keep_days: days,
                    target_keep_days: days,
                },
                None => settings.clocks,
            },
            dry: self.dry,
        }
    }
}

impl SweepReport {
    pub fn run(&self) -> i32 {
        let scratch = &self.scratch;
        if !scratch.is_dir() {
            SweepReport::say(&format!(
                "dibs: nothing at {} to sweep.\n",
                scratch.display()
            ));
            return 0;
        }
        let now = Moment::epoch_now();
        let mut tally = Totals::default();
        SweepReport::say(&format!(
            "dibs --gc on {}, under {}\n",
            self.host,
            scratch.display()
        ));
        let removal = Removal::new(None, &SweepReport::say);
        let sweep = Sweep::new(scratch, &self.home, self.clocks, removal);
        let sweep = match self.dry {
            true => sweep.dry(),
            false => sweep,
        };
        for kind in Swept::ALL {
            let entries = sweep.entries(kind);
            let mapped = kind == Swept::Caches && Host::shares_blocks(&scratch.join("target"));
            let Measured { sizes, sharing } = tally.measure(&entries, mapped);
            if let Some(sharing) = &sharing {
                tally.total = tally.total - sizes.sum() + sharing.together();
            }
            let Some(mut section) = sweep.judged(kind, now) else {
                SweepReport::say(
                    "  runners: a build of one is running, so none is collected now\n",
                );
                continue;
            };
            sweep.collect(&mut section, now);
            let listed = match kind {
                Swept::Trees => self.trees(&section, &sizes, now, &mut tally),
                Swept::Caches => self.caches(&section, &sizes, sharing.as_ref(), now, &mut tally),
                Swept::Jobs => {
                    self.bulk("job logs and artifacts", &section, &sizes, now, &mut tally)
                }
                Swept::Leftovers => self.bulk(
                    "leftover temporary files",
                    &section,
                    &sizes,
                    now,
                    &mut tally,
                ),
                Swept::Results => {
                    self.bulk("results kept under out", &section, &sizes, now, &mut tally)
                }
                Swept::Runners => self.runners(&section, &sizes, &mut tally),
            };
            SweepReport::say(&listed);
        }
        self.others(now, &mut tally);
        match self.dry {
            true => SweepReport::say(&format!(
                "  {} of {} is past its clock and would go. Run it without --dry-run.\n",
                Kib(tally.would),
                Kib(tally.total)
            )),
            false => SweepReport::say(&format!(
                "  reclaimed {} of {}\n",
                Kib(tally.freed),
                Kib(tally.total)
            )),
        }
        let mut mounts = Vec::new();
        for dir in [scratch.clone(), scratch.join("target")] {
            if let Some(free) = Free::of(&dir).filter(|f| !mounts.contains(&f.mount)) {
                SweepReport::say(&format!(
                    "  {} free of {} on {}\n",
                    Bytes(free.available),
                    Bytes(free.size),
                    free.mount.display()
                ));
                mounts.push(free.mount);
            }
        }
        0
    }

    fn verdict(&self, verdict: &Verdict) -> &'static str {
        match (verdict.fate, verdict.removed, self.dry) {
            (_, true, _) => "   removed",
            (Fate::Past, false, true) => "   would remove",
            (Fate::Past, false, false) => "   not all removed",
            (Fate::Held, ..) => "   held by a build",
            (Fate::Preparing, ..) => "   held by a prepare",
            _ => "",
        }
    }

    /// What went, or would, added to the tally.
    fn count(&self, verdict: &Verdict, kib: u64, tally: &mut Totals) -> bool {
        let past = verdict.fate == Fate::Past;
        if verdict.removed {
            tally.freed += kib;
        } else if past && self.dry {
            tally.would += kib;
        }
        past
    }

    fn trees(&self, section: &Section, sizes: &Sizes, now: u64, tally: &mut Totals) -> String {
        let mut rows = Rows::default();
        for verdict in &section.verdicts {
            let kib = sizes.of(&verdict.path);
            rows.push(Row {
                kib,
                past: self.count(verdict, kib, tally),
                line: format!(
                    "    {:<40} {:>7}  used {}{}",
                    self.shown(&verdict.path),
                    Kib(kib),
                    SweepReport::ago(now, verdict.used),
                    self.verdict(verdict)
                ),
            });
        }
        rows.out(&format!(
            "  worktrees, removed after {} days unused",
            self.clocks.keep_days
        ))
    }

    /// A cache seeded from a sibling shares the sibling's blocks, which a plain count gives to
    /// both; where the filesystem shares blocks, each cache's own are what removing it frees.
    fn caches(
        &self,
        section: &Section,
        sizes: &Sizes,
        sharing: Option<&Sharing>,
        now: u64,
        tally: &mut Totals,
    ) -> String {
        let mut rows = Rows::default();
        for verdict in section.verdicts.iter().filter(|v| v.fate != Fate::Hollow) {
            let own = sharing.and_then(|s| s.own.get(&verdict.path).copied());
            let kib = own.unwrap_or_else(|| sizes.of(&verdict.path));
            let own = own
                .map(|own| format!("  own {:>7}", Kib(own)))
                .unwrap_or_default();
            rows.push(Row {
                kib,
                past: self.count(verdict, kib, tally),
                line: format!(
                    "    {:<40} {:>7}{own}  used {}{}",
                    self.shown(&verdict.path),
                    Kib(sizes.of(&verdict.path)),
                    SweepReport::ago(now, verdict.used),
                    self.verdict(verdict)
                ),
            });
        }
        let target = self.scratch.join("target");
        let mount = Free::mount_of(&target)
            .map(|m| m.display().to_string())
            .unwrap_or_default();
        let mut listed = rows.out(&format!(
            "  build caches on {mount}, removed after {} days unused",
            self.clocks.target_keep_days
        ));
        if let Some(sharing) = sharing
            && !sharing.own.is_empty()
        {
            listed.push_str(&format!(
                "    together {} on the disk: own is what removing that cache alone frees, and {} is shared among them\n",
                Kib(sharing.together()),
                Kib(sharing.shared)
            ));
        }
        listed
    }

    /// Counted rather than listed: they are alike and there are hundreds, and the one anybody
    /// wants is found by its id with `dibs out`.
    fn bulk(
        &self,
        what: &str,
        section: &Section,
        sizes: &Sizes,
        now: u64,
        tally: &mut Totals,
    ) -> String {
        let (mut kib, mut oldest) = (0, 0);
        let (mut past, mut past_kib, mut removed, mut removed_kib) = (0, 0, 0, 0);
        let count = section.verdicts.len();
        for verdict in &section.verdicts {
            let k = sizes.of(&verdict.path);
            kib += k;
            oldest = oldest.max(Clocks::days(now, verdict.used));
            if self.count(verdict, k, tally) {
                past += 1;
                past_kib += k;
            }
            if verdict.removed {
                removed += 1;
                removed_kib += k;
            }
        }
        if count == 0 {
            return String::new();
        }
        let ending = match (past, self.dry) {
            (0, _) => ", none past its clock".to_string(),
            (_, true) => format!(", {past} past it holding {}, which would go", Kib(past_kib)),
            (_, false) if removed == past => {
                format!(", removed {removed} holding {}", Kib(removed_kib))
            }
            (_, false) => format!(
                ", removed {removed} holding {}, and could not remove {}",
                Kib(removed_kib),
                past - removed
            ),
        };
        format!(
            "  {what}, removed after {} days: {count} {}, {}, oldest {oldest} days{ending}\n",
            self.clocks.keep_days,
            match count {
                1 => "entry",
                _ => "entries",
            },
            Kib(kib)
        )
    }

    fn runners(&self, section: &Section, sizes: &Sizes, tally: &mut Totals) -> String {
        let dir = Runners::in_home(&self.home).dir().to_path_buf();
        let mut rows = Rows::default();
        for verdict in &section.verdicts {
            let kib = sizes.of(&verdict.path);
            rows.push(Row {
                kib,
                past: self.count(verdict, kib, tally),
                line: format!(
                    "    {:<40} {:>7}{}",
                    verdict
                        .path
                        .strip_prefix(&dir)
                        .unwrap_or(&verdict.path)
                        .display(),
                    Kib(kib),
                    self.verdict(verdict)
                ),
            });
        }
        rows.out(&format!(
            "  runners in {}, replaced ones removed after {} days unused",
            dir.display(),
            self.clocks.keep_days
        ))
    }

    /// Nothing dibs made, so nothing dibs deletes: a directory written by hand may be the only
    /// copy of somebody's work. Sized and dated, so they can be asked.
    fn others(&self, now: u64, tally: &mut Totals) {
        let others: Vec<PathBuf> = self
            .scratch
            .entries()
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| !DIBS_OWN.iter().any(|own| n == *own))
            })
            .collect();
        if others.is_empty() {
            return;
        }
        let sizes = tally.measure(&others, false).sizes;
        let mut rows = Rows::default();
        for other in &others {
            let kib = sizes.of(other);
            rows.push(Row {
                kib,
                past: false,
                line: format!(
                    "    {:<40} {:>7}  written {}",
                    self.shown(other),
                    Kib(kib),
                    SweepReport::ago(now, other.written(now))
                ),
            });
        }
        SweepReport::say(&rows.out("  not dibs's, never removed by this"));
    }

    /// A path as it is under scratch.
    fn shown(&self, path: &Path) -> String {
        path.strip_prefix(&self.scratch)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

impl Rows {
    fn push(&mut self, row: Row) {
        self.rows.push(row);
    }

    /// The heading and its rows, nothing when there are none.
    fn out(mut self, heading: &str) -> String {
        if self.rows.is_empty() {
            return String::new();
        }
        self.rows
            .sort_by(|a, b| b.kib.cmp(&a.kib).then_with(|| b.line.cmp(&a.line)));
        let mut said = format!("{heading}\n");
        let (mut rest, mut rest_kib) = (0, 0);
        for (at, row) in self.rows.iter().enumerate() {
            match at < LISTED || row.past {
                true => said.push_str(&format!("{}\n", row.line)),
                false => {
                    rest += 1;
                    rest_kib += row.kib;
                }
            }
        }
        if rest > 0 {
            said.push_str(&format!(
                "    and {rest} more holding {}, none of it past its clock\n",
                Kib(rest_kib)
            ));
        }
        said
    }
}

impl Sharing {
    /// What they hold on the disk together, shared blocks once.
    fn together(&self) -> u64 {
        self.shared + self.own.values().sum::<u64>()
    }
}

/// Free space where a directory lives.
struct Free {
    available: u64,
    size: u64,
    mount: PathBuf,
}

impl Free {
    fn of(dir: &Path) -> Option<Free> {
        let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: statvfs is plain data, which statvfs fills from a NUL-terminated path.
        let mut found: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(path.as_ptr(), &mut found) } != 0 {
            return None;
        }
        let unit = found.f_frsize as u64;
        Some(Free {
            available: found.f_bavail as u64 * unit,
            size: found.f_blocks as u64 * unit,
            mount: Free::mount_of(dir)?,
        })
    }
}

impl Free {
    /// Where the filesystem holding a directory is mounted: the highest directory above it on
    /// the same device.
    fn mount_of(dir: &Path) -> Option<PathBuf> {
        let dir = dir.canonicalize().ok()?;
        let device = fs::metadata(&dir).ok()?.dev();
        let mut mount = dir.clone();
        for above in dir.ancestors().skip(1) {
            match fs::metadata(above) {
                Ok(meta) if meta.dev() == device => mount = above.to_path_buf(),
                _ => break,
            }
        }
        Some(mount)
    }
}

impl Totals {
    /// What `du -sk` gives each path, a file linked twice counted once, added to the total; and,
    /// when `mapped`, what each holds alone.
    fn measure(&mut self, paths: &[PathBuf], mapped: bool) -> Measured {
        let mut linked = HashSet::new();
        let mut sizes = HashMap::new();
        let mut own = HashMap::new();
        let mut shared = HashMap::new();
        for (path, walked) in paths.iter().zip(Walked::all(paths, mapped)) {
            let mut blocks = walked.blocks;
            for (file, held) in walked.linked {
                if !linked.insert(file) {
                    blocks -= held;
                }
            }
            sizes.insert(path.clone(), blocks.div_ceil(2));
            own.insert(path.clone(), walked.alone);
            shared.extend(walked.shared);
        }
        self.total += sizes.values().sum::<u64>();
        Measured {
            sizes: Sizes(sizes),
            sharing: mapped.then(|| Sharing {
                own,
                shared: shared.values().sum(),
            }),
        }
    }
}

impl Walked {
    /// Each path's walk, in order, the paths spread over the cores.
    fn all(paths: &[PathBuf], mapped: bool) -> Vec<Walked> {
        let next = AtomicUsize::new(0);
        let cores = thread::available_parallelism().map_or(1, NonZero::get);
        let mut walked: Vec<(usize, Walked)> = thread::scope(|scope| {
            let walkers: Vec<_> = (0..cores.min(paths.len()))
                .map(|_| {
                    scope.spawn(|| {
                        iter::from_fn(|| {
                            let at = next.fetch_add(1, Ordering::Relaxed);
                            paths.get(at).map(|path| (at, Walked::of(path, mapped)))
                        })
                        .collect::<Vec<_>>()
                    })
                })
                .collect();
            walkers
                .into_iter()
                .flat_map(|w| w.join().unwrap_or_else(|panic| panic::resume_unwind(panic)))
                .collect()
        });
        walked.sort_by_key(|(at, _)| *at);
        walked.into_iter().map(|(_, w)| w).collect()
    }

    /// Everything under a path, its own entry included.
    fn of(path: &Path, mapped: bool) -> Walked {
        let mut walked = Walked::default();
        let Ok(meta) = fs::symlink_metadata(path) else {
            return walked;
        };
        walked.add(path, &meta, mapped);
        let mut pending = match meta.is_dir() {
            true => vec![path.to_path_buf()],
            false => Vec::new(),
        };
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let path = entry.path();
                walked.add(&path, &meta, mapped);
                if meta.is_dir() {
                    pending.push(path);
                }
            }
        }
        walked
    }

    fn add(&mut self, path: &Path, meta: &fs::Metadata, mapped: bool) {
        if meta.nlink() > 1
            && !meta.is_dir()
            && self
                .linked
                .insert((meta.dev(), meta.ino()), meta.blocks())
                .is_some()
        {
            return;
        }
        self.blocks += meta.blocks();
        if !mapped || !meta.is_file() {
            return;
        }
        for extent in Host::extents(path).unwrap_or_default() {
            match extent.shared {
                true => {
                    self.shared
                        .insert(extent.physical / 1024, extent.length / 1024);
                }
                false => self.alone += extent.length / 1024,
            }
        }
    }
}

/// KiB under each path measured.
struct Sizes(HashMap<PathBuf, u64>);

impl Sizes {
    fn of(&self, path: &Path) -> u64 {
        self.0.get(path).copied().unwrap_or_default()
    }

    fn sum(&self) -> u64 {
        self.0.values().sum()
    }
}

/// KiB as the sizes a person acts on.
struct Kib(u64);

impl fmt::Display for Kib {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kib = self.0;
        f.pad(&match kib {
            1_048_576.. => format!("{}.{}G", kib / 1_048_576, kib % 1_048_576 * 10 / 1_048_576),
            1024.. => format!("{}M", kib / 1024),
            _ => format!("{kib}K"),
        })
    }
}

/// Bytes as `df -h` gives them, rounded up.
pub struct Bytes(pub u64);

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0;
        if bytes < 1024 {
            return f.pad(&bytes.to_string());
        }
        let units = ['K', 'M', 'G', 'T', 'P', 'E'];
        let mut value = bytes as f64 / 1024.0;
        let mut unit = 0;
        while value >= 1024.0 && unit < units.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }
        f.pad(&match (value * 10.0).ceil() / 10.0 {
            tenths if tenths < 10.0 => format!("{tenths:.1}{}", units[unit]),
            _ => format!("{}{}", value.ceil(), units[unit]),
        })
    }
}

impl SweepReport {
    fn ago(now: u64, when: u64) -> String {
        match Clocks::days(now, when) {
            0 => "today".to_string(),
            1 => "yesterday".to_string(),
            days => format!("{days} days ago"),
        }
    }

    /// A section as it is ready, for whoever reads the job's output as it comes.
    fn say(text: &str) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Span;

    #[test]
    fn sizes_read_as_the_sweep_has_always_printed_them() {
        assert_eq!(Kib(512).to_string(), "512K");
        assert_eq!(Kib(2048).to_string(), "2M");
        assert_eq!(Kib(1_572_864).to_string(), "1.5G");
        assert_eq!(Bytes(10 * 1024 * 1024 * 1024 + 1).to_string(), "11G");
        assert_eq!(Bytes(9 * 1024 * 1024 * 1024 + 1).to_string(), "9.1G");
        assert_eq!(SweepReport::ago(Span::DAY.0 * 3, 0), "3 days ago");
    }

    #[test]
    fn past_twenty_rows_the_rest_are_counted_but_a_doomed_one_is_named() {
        let mut rows = Rows::default();
        for n in 0..25u64 {
            rows.push(Row {
                kib: 100 + n,
                past: n == 0,
                line: format!("row {n}"),
            });
        }
        let said = rows.out("heading");
        assert!(said.contains("row 0\n"));
        assert!(said.contains("and 4 more holding"));
        assert_eq!(said.lines().count(), 23);
    }
}
