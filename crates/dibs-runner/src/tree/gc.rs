//! `dibs --gc`: what fills a machine is its own worktrees, build caches and job logs, swept by age.
//! It runs as a shared job, so the deleting queues behind a measurement rather than competing
//! with one, and the log carries what it reclaimed.

use crate::{
    clock::{Moment, Span},
    itself::Itself,
    machine::Site,
    platform::{Host, Platform as _},
    settings::{Settings, home},
    tree::{
        clocks::{Clocks, Contents as _, Dates as _, Fate, Removal},
        copy::Reflinks,
        runners::Runners,
        spread::Spread as _,
        sweep::{Section, Sweep, Swept, Verdict},
    },
};
use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    fmt, fs,
    io::{self, Write as _},
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

/// Rows listed under a heading before the rest are counted; what is past its clock always is.
const LISTED: usize = 20;
/// How often a section still going says how far it is.
const PROGRESS_EVERY: Duration = Duration::from_secs(10);
const BAR_WIDTH: usize = 20;
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
    reflinks: Reflinks,
    /// Only say what would go.
    dry: bool,
}

/// KiB reclaimed and past their clocks.
#[derive(Default)]
struct Totals {
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

/// What removing each mapped build cache alone would free, where blocks are shared.
struct Sharing {
    own: HashMap<PathBuf, u64>,
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
            reflinks: settings.reflinks,
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
        let began = Instant::now();
        let mut took = Vec::new();
        for kind in Swept::ALL {
            let section_began = Instant::now();
            let entries = sweep.entries(kind);
            // Mapping a file's extents makes btrfs walk every other reference to each of them,
            // which over caches seeded from each other takes minutes, so only what may go is.
            let mapped = kind == Swept::Caches
                && self.reflinks != Reflinks::Never
                && Host::shares_blocks(&scratch.join("target"));
            let Measured { sizes, sharing } = tally.measure(kind.named(), &entries, |cache| {
                mapped && sweep.may_go(cache, now)
            });
            let Some(mut section) = sweep.judged(kind, now) else {
                SweepReport::say(
                    "  runners: a build of one is running, so none is collected now\n",
                );
                continue;
            };
            let past = section.verdicts.iter().filter(|v| v.fate == Fate::Past);
            let (count, kib) = past.fold((0, 0), |(n, k), v| (n + 1, k + sizes.of(&v.path)));
            let (gone, gone_kib) = (AtomicUsize::new(0), AtomicU64::new(0));
            let removing = Instant::now();
            match self.dry {
                true => sweep.collect(&mut section, now),
                false => ticking(
                    || {
                        SweepReport::say(&Progress::removing(
                            kind.named(),
                            (gone.load(Ordering::Relaxed), count),
                            (gone_kib.load(Ordering::Relaxed), kib),
                            removing.elapsed(),
                        ))
                    },
                    || {
                        sweep.collect_saying(&mut section, now, &|path| {
                            gone.fetch_add(1, Ordering::Relaxed);
                            gone_kib.fetch_add(sizes.of(path), Ordering::Relaxed);
                        })
                    },
                ),
            }
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
            took.push((kind, section_began.elapsed()));
            SweepReport::say(&listed);
        }
        let others_began = Instant::now();
        self.others(now, &mut tally);
        let others = others_began.elapsed();
        match self.dry {
            true => SweepReport::say(&format!(
                "  {} is past its clock and would go. Run it without --dry-run.\n",
                Kib(tally.would)
            )),
            false => SweepReport::say(&format!("  reclaimed {}\n", Kib(tally.freed))),
        }
        SweepReport::say(&format!(
            "  took {}: {}, not dibs's {}\n",
            Span(began.elapsed().as_secs()),
            took.iter()
                .map(|(kind, spent)| format!("{} {}", kind.named(), Span(spent.as_secs())))
                .collect::<Vec<_>>()
                .join(", "),
            Span(others.as_secs())
        ));
        let mut mounts = Vec::new();
        for dir in [scratch.clone(), scratch.join("target")] {
            if let Some(free) = Free::of(&dir).filter(|f| !mounts.contains(&f.mount)) {
                SweepReport::say(&format!(
                    "  {} in use and {} free of {} on {}\n",
                    Bytes(free.used),
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
        if sharing.is_some_and(|s| !s.own.is_empty()) {
            listed.push_str(
                "    a cache's size counts blocks it shares with others; own is what removing it alone frees\n",
            );
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
        let sizes = tally.measure("not dibs's", &others, |_| false).sizes;
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

/// Free space where a directory lives.
struct Free {
    available: u64,
    used: u64,
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
            used: (found.f_blocks - found.f_bfree) as u64 * unit,
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
    /// for each path `mapped` names, what it holds alone.
    fn measure(&self, what: &str, paths: &[PathBuf], mapped: impl Fn(&Path) -> bool) -> Measured {
        let mut linked = HashSet::new();
        let mut sizes = HashMap::new();
        let mut own = HashMap::new();
        let asked: Vec<(&PathBuf, bool)> = paths.iter().map(|p| (p, mapped(p))).collect();
        let done = AtomicUsize::new(0);
        let began = Instant::now();
        let walked = ticking(
            || {
                SweepReport::say(&Progress::line(
                    what,
                    done.load(Ordering::Relaxed),
                    asked.len(),
                    began.elapsed(),
                ))
            },
            || {
                asked.spread(|&(path, mapped)| {
                    let walked = Walked::of(path, mapped);
                    done.fetch_add(1, Ordering::Relaxed);
                    walked
                })
            },
        );
        for (&(path, mapped), walked) in asked.iter().zip(walked) {
            let mut blocks = walked.blocks;
            for (file, held) in walked.linked {
                if !linked.insert(file) {
                    blocks -= held;
                }
            }
            sizes.insert(path.clone(), blocks.div_ceil(2));
            if mapped {
                own.insert(path.clone(), walked.alone);
            }
        }
        Measured {
            sizes: Sizes(sizes),
            sharing: (!own.is_empty()).then_some(Sharing { own }),
        }
    }
}

impl Walked {
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
            if !extent.shared {
                self.alone += extent.length / 1024;
            }
        }
    }
}

/// `work`, with `tick` called every `PROGRESS_EVERY` until it returns.
fn ticking<R>(tick: impl Fn() + Send, work: impl FnOnce() -> R) -> R {
    let (finished, ticks) = mpsc::channel::<()>();
    thread::scope(|scope| {
        scope.spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = ticks.recv_timeout(PROGRESS_EVERY) {
                tick();
            }
        });
        let result = work();
        drop(finished);
        result
    })
}

/// How far a section's walk is, for whoever reads the job's output as it comes.
struct Progress;

impl Progress {
    fn line(what: &str, done: usize, total: usize, spent: Duration) -> String {
        format!(
            "    {what}: {} {done} of {total} measured in {}, {}\n",
            Progress::bar(done as u64, total as u64),
            Span(spent.as_secs()),
            Progress::left(done as u64, total as u64, spent)
        )
    }

    /// Paced by size rather than by count: one cache can hold a hundred times another.
    fn removing(
        what: &str,
        (done, total): (usize, usize),
        (kib, of): (u64, u64),
        spent: Duration,
    ) -> String {
        format!(
            "    {what}: {} removed {done} of {total}, {} of {}, in {}, {}\n",
            Progress::bar(kib, of),
            Kib(kib),
            Kib(of),
            Span(spent.as_secs()),
            Progress::left(kib, of, spent)
        )
    }

    fn bar(done: u64, total: u64) -> String {
        let filled = (done.min(total) * BAR_WIDTH as u64)
            .checked_div(total)
            .unwrap_or(BAR_WIDTH as u64) as usize;
        format!("[{}{}]", "#".repeat(filled), ".".repeat(BAR_WIDTH - filled))
    }

    /// The time so far spread over what is done, so none until something is.
    fn left(done: u64, total: u64, spent: Duration) -> String {
        match done {
            0 => "no estimate until one is done".to_string(),
            _ => format!(
                "about {} left",
                Span(
                    (spent.as_secs_f64() * total.saturating_sub(done) as f64 / done as f64) as u64
                )
            ),
        }
    }
}

/// KiB under each path measured.
struct Sizes(HashMap<PathBuf, u64>);

impl Sizes {
    fn of(&self, path: &Path) -> u64 {
        self.0.get(path).copied().unwrap_or_default()
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
    fn progress_says_how_far_and_how_long_is_left_once_it_can_tell() {
        assert_eq!(
            Progress::line("build caches", 0, 4, Duration::from_secs(10)),
            "    build caches: [....................] 0 of 4 measured in 10s, no estimate until one is done\n"
        );
        assert_eq!(
            Progress::line("build caches", 1, 4, Duration::from_secs(60)),
            "    build caches: [#####...............] 1 of 4 measured in 1m00s, about 3m00s left\n"
        );
        assert_eq!(
            Progress::removing(
                "build caches",
                (1, 3),
                (1_048_576, 4_194_304),
                Duration::from_secs(30)
            ),
            "    build caches: [#####...............] removed 1 of 3, 1.0G of 4.0G, in 30s, about 1m30s left\n"
        );
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
