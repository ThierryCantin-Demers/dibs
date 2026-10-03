//! `dibs --gc`: what fills a machine is its own worktrees, build caches and job logs, swept by age.
//! It runs as a shared job, so the deleting queues behind a measurement rather than competing
//! with one, and the log carries what it reclaimed.

use crate::{
    clock::Moment,
    machine::Machine,
    platform::{Host, Platform as _},
    settings::{Settings, home},
};
use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    fs,
    io::{self, Write as _},
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const DAY: u64 = 86400;
/// Rows listed under a heading before the rest are counted; what is past its clock always is.
const LISTED: usize = 20;
/// Directories under scratch that dibs makes; anything else is somebody's and never removed.
const DIBS_OWN: [&str; 7] = ["ws", "target", "jobs", "tmp", "out", "runner", "run"];
/// The marker a prepare leaves, whose time is when a tree was last used.
const USED: &str = ".dibs-used";
/// The word the client's own binary serves the runner under.
const RUNNER_WORD: &str = "__runner";

/// One sweep of a scratch directory.
pub struct Sweep {
    pub scratch: PathBuf,
    /// Where the clones live that a removed worktree was added from.
    pub prog: PathBuf,
    pub host: String,
    /// Days a worktree, a job's directory or a temporary file is kept unused.
    pub keep: u64,
    pub target_keep: u64,
    /// Only say what would go.
    pub dry: bool,
}

/// KiB measured, reclaimed and past their clocks.
#[derive(Default)]
struct Tally {
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

    /// The job that sweeps: this runner again, as it was started.
    pub fn command(&self) -> String {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("dibs-runner"));
        let mut words = vec![exe.display().to_string()];
        if std::env::args().nth(1).as_deref() == Some(RUNNER_WORD) {
            words.push(RUNNER_WORD.to_string());
        }
        words.push("gc".to_string());
        words.push(self.days.map_or("default".to_string(), |d| d.to_string()));
        words.push(u8::from(self.dry).to_string());
        words
            .iter()
            .map(|w| format!("'{}'", w.replace('\'', r"'\''")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The sweep, with the machine's own clocks unless days were given.
    pub fn sweep(&self) -> Sweep {
        let settings = Settings::from_env();
        let machine = Machine::set_up().ok();
        Sweep {
            scratch: machine
                .as_ref()
                .map_or_else(|| home().join(".cache/dibs"), |m| m.scratch.clone()),
            prog: home().join("prog"),
            host: crate::machine::short_hostname(),
            keep: self.days.unwrap_or(settings.keep_days),
            target_keep: self.days.unwrap_or(settings.target_keep_days),
            dry: self.dry,
        }
    }
}

impl Sweep {
    pub fn run(&self) -> i32 {
        let scratch = &self.scratch;
        if !scratch.is_dir() {
            say(&format!(
                "dibs: nothing at {} to sweep.\n",
                scratch.display()
            ));
            return 0;
        }
        let now = Moment::epoch_now();
        let mut tally = Tally::default();
        say(&format!(
            "dibs --gc on {}, under {}\n",
            self.host,
            scratch.display()
        ));
        self.worktrees(now, &mut tally);
        self.caches(now, &mut tally);
        let jobs = entries(&scratch.join("jobs"));
        self.bulk(now, "job logs and artifacts", &jobs, &mut tally);
        let leftovers = [entries(&scratch.join("tmp")), entries(&scratch.join("out"))].concat();
        self.bulk(now, "leftover temporary files", &leftovers, &mut tally);
        self.others(now, &mut tally);
        match self.dry {
            true => say(&format!(
                "  {} of {} is past its clock and would go. Run it without --dry-run.\n",
                size(tally.would),
                size(tally.total)
            )),
            false => say(&format!(
                "  reclaimed {} of {}\n",
                size(tally.freed),
                size(tally.total)
            )),
        }
        let mut mounts = Vec::new();
        for dir in [scratch.clone(), scratch.join("target")] {
            if let Some(free) = Free::of(&dir).filter(|f| !mounts.contains(&f.mount)) {
                say(&format!(
                    "  {} free of {} on {}\n",
                    human(free.available),
                    human(free.size),
                    free.mount.display()
                ));
                mounts.push(free.mount);
            }
        }
        0
    }

    fn past(&self, now: u64, when: u64, clock: u64) -> bool {
        now.saturating_sub(when) / DAY > clock
    }

    fn verdict(&self, past: bool) -> &'static str {
        match (past, self.dry) {
            (false, _) => "",
            (true, true) => "   would remove",
            (true, false) => "   removed",
        }
    }

    /// A worktree is git's to remove, and one git has lost is a plain directory; the clone it was
    /// added from keeps a registration either way, which the prune clears.
    fn worktrees(&self, now: u64, tally: &mut Tally) {
        let trees: Vec<PathBuf> = entries(&self.scratch.join("ws"))
            .iter()
            .flat_map(|repo| entries(repo))
            .filter(|tree| tree.is_dir())
            .collect();
        let sizes = measure(&trees, tally);
        let mut rows = Rows::default();
        let mut pruned = Vec::new();
        for tree in &trees {
            let marker = tree.join(USED);
            if !marker.exists() {
                let _ = fs::write(&marker, "");
            }
            let used = used(tree, now);
            let kib = sizes.get(tree).copied().unwrap_or_default();
            let past = self.past(now, used, self.keep);
            if past && self.dry {
                tally.would += kib;
            } else if past {
                let removed = Command::new("git")
                    .arg("-C")
                    .arg(tree)
                    .args(["worktree", "remove", "--force"])
                    .arg(tree)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success());
                if !removed {
                    let _ = fs::remove_dir_all(tree);
                }
                tally.freed += kib;
                if let Some(repo) = tree.parent().and_then(Path::file_name) {
                    pruned.push(repo.to_owned());
                }
            }
            rows.push(Row {
                kib,
                past,
                line: format!(
                    "    {:<40} {:>7}  used {}{}",
                    self.shown(tree),
                    size(kib),
                    ago(now, used),
                    self.verdict(past)
                ),
            });
        }
        say(&rows.out(&format!(
            "  worktrees, removed after {} days unused",
            self.keep
        )));
        pruned.sort();
        pruned.dedup();
        for repo in pruned {
            let _ = Command::new("git")
                .arg("-C")
                .arg(self.prog.join(repo))
                .args(["worktree", "prune"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    /// A cache seeded from a sibling shares the sibling's blocks, which a plain count gives to
    /// both; where the filesystem shares blocks, each cache's own are what removing it frees.
    fn caches(&self, now: u64, tally: &mut Tally) {
        let target = self.scratch.join("target");
        let caches: Vec<PathBuf> = entries(&target)
            .into_iter()
            .filter(|c| c.is_dir())
            .collect();
        let before = tally.total;
        let sizes = measure(&caches, tally);
        let sharing = Host::shares_blocks(&target).then(|| Sharing::of(&caches));
        let together = sharing
            .as_ref()
            .map(|s| s.shared + s.own.values().sum::<u64>());
        if let Some(together) = together {
            tally.total = before + together;
        }
        let mut rows = Rows::default();
        for cache in &caches {
            let marker = cache.join(USED);
            if !marker.exists() {
                let _ = fs::write(&marker, "swept\n");
            }
            let used = used(cache, now);
            let own = sharing.as_ref().and_then(|s| s.own.get(cache).copied());
            let kib = own.unwrap_or_else(|| sizes.get(cache).copied().unwrap_or_default());
            let past = self.past(now, used, self.target_keep);
            if past && self.dry {
                tally.would += kib;
            } else if past {
                let _ = fs::remove_dir_all(cache);
                tally.freed += kib;
            }
            let own = own
                .map(|own| format!("  own {:>7}", size(own)))
                .unwrap_or_default();
            rows.push(Row {
                kib,
                past,
                line: format!(
                    "    {:<40} {:>7}{own}  used {}{}",
                    self.shown(cache),
                    size(sizes.get(cache).copied().unwrap_or_default()),
                    ago(now, used),
                    self.verdict(past)
                ),
            });
        }
        let mount = mount_of(&target)
            .map(|m| m.display().to_string())
            .unwrap_or_default();
        say(&rows.out(&format!(
            "  build caches on {mount}, removed after {} days unused",
            self.target_keep
        )));
        if let (Some(sharing), Some(together)) = (&sharing, together)
            && !sharing.own.is_empty()
        {
            say(&format!(
                "    together {} on the disk: own is what removing that cache alone frees, and {} is shared among them\n",
                size(together),
                size(sharing.shared)
            ));
        }
    }

    /// Counted rather than listed: they are alike and there are hundreds, and the one anybody
    /// wants is found by its id with `dibs out`.
    fn bulk(&self, now: u64, what: &str, paths: &[PathBuf], tally: &mut Tally) {
        let sizes = measure(paths, tally);
        let (mut count, mut kib, mut past, mut past_kib, mut oldest) = (0, 0, 0, 0, 0);
        for path in paths {
            let Ok(meta) = fs::symlink_metadata(path) else {
                continue;
            };
            let k = sizes.get(path).copied().unwrap_or_default();
            count += 1;
            kib += k;
            let age = now.saturating_sub(meta.mtime().max(0) as u64) / DAY;
            oldest = oldest.max(age);
            if age <= self.keep {
                continue;
            }
            past += 1;
            past_kib += k;
            match self.dry {
                true => tally.would += k,
                false => {
                    let gone = match meta.is_dir() {
                        true => fs::remove_dir_all(path),
                        false => fs::remove_file(path),
                    };
                    if gone.is_ok() {
                        tally.freed += k;
                    }
                }
            }
        }
        if count == 0 {
            return;
        }
        let ending = match (past, self.dry) {
            (0, _) => ", none past its clock".to_string(),
            (_, true) => format!(
                ", {past} past it holding {}, which would go",
                size(past_kib)
            ),
            (_, false) => format!(", removed {past} holding {}", size(past_kib)),
        };
        say(&format!(
            "  {what}, removed after {} days: {count} {}, {}, oldest {oldest} days{ending}\n",
            self.keep,
            match count {
                1 => "entry",
                _ => "entries",
            },
            size(kib)
        ));
    }

    /// Nothing dibs made, so nothing dibs deletes: a directory written by hand may be the only
    /// copy of somebody's work. Sized and dated, so they can be asked.
    fn others(&self, now: u64, tally: &mut Tally) {
        let others: Vec<PathBuf> = entries(&self.scratch)
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| !DIBS_OWN.iter().any(|own| n == *own))
            })
            .collect();
        if others.is_empty() {
            return;
        }
        let sizes = measure(&others, tally);
        let mut rows = Rows::default();
        for other in &others {
            let kib = sizes.get(other).copied().unwrap_or_default();
            let written = fs::symlink_metadata(other).map_or(now, |m| m.mtime().max(0) as u64);
            rows.push(Row {
                kib,
                past: false,
                line: format!(
                    "    {:<40} {:>7}  written {}",
                    self.shown(other),
                    size(kib),
                    ago(now, written)
                ),
            });
        }
        say(&rows.out("  not dibs's, never removed by this"));
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
                size(rest_kib)
            ));
        }
        said
    }
}

impl Sharing {
    /// Each cache's blocks no other file shares, and the shared ones counted once, by where they
    /// sit on the disk.
    fn of(caches: &[PathBuf]) -> Sharing {
        let mut own = HashMap::new();
        let mut shared: HashMap<u64, u64> = HashMap::new();
        for cache in caches {
            let mut seen = HashSet::new();
            let mut alone = 0;
            for file in files(cache) {
                let Ok(meta) = fs::symlink_metadata(&file) else {
                    continue;
                };
                if !seen.insert(meta.ino()) {
                    continue;
                }
                for extent in Host::extents(&file).unwrap_or_default() {
                    match extent.shared {
                        true => {
                            shared.insert(extent.physical / 1024, extent.length / 1024);
                        }
                        false => alone += extent.length / 1024,
                    }
                }
            }
            own.insert(cache.clone(), alone);
        }
        Sharing {
            own,
            shared: shared.values().sum(),
        }
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
            mount: mount_of(dir)?,
        })
    }
}

/// Where the filesystem holding a directory is mounted: the highest directory above it on the
/// same device.
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

/// The entries of a directory, sorted as the shell's glob lists them.
fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// Every regular file below a directory.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).into_iter().flatten().flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(entry.path()),
                Ok(kind) if kind.is_file() => found.push(entry.path()),
                _ => {}
            }
        }
    }
    found
}

/// What `du -sk` gives each path, a file linked twice counted once, added to the total.
fn measure(paths: &[PathBuf], tally: &mut Tally) -> HashMap<PathBuf, u64> {
    let mut seen = HashSet::new();
    let sizes: HashMap<PathBuf, u64> = paths
        .iter()
        .map(|path| (path.clone(), allocated(path, &mut seen).div_ceil(2)))
        .collect();
    tally.total += sizes.values().sum::<u64>();
    sizes
}

/// 512-byte blocks allocated under a path, its own included.
fn allocated(path: &Path, seen: &mut HashSet<(u64, u64)>) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.nlink() > 1 && !meta.is_dir() && !seen.insert((meta.dev(), meta.ino())) {
        return 0;
    }
    let below: u64 = match meta.is_dir() {
        true => fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| allocated(&e.path(), seen))
            .sum(),
        false => 0,
    };
    meta.blocks() + below
}

/// When a tree was last used: its marker's time, else its own.
fn used(dir: &Path, now: u64) -> u64 {
    fs::metadata(dir.join(USED))
        .or_else(|_| fs::metadata(dir))
        .map_or(now, |m| m.mtime().max(0) as u64)
}

/// KiB as the sizes a person acts on.
fn size(kib: u64) -> String {
    match kib {
        1_048_576.. => format!("{}.{}G", kib / 1_048_576, kib % 1_048_576 * 10 / 1_048_576),
        1024.. => format!("{}M", kib / 1024),
        _ => format!("{kib}K"),
    }
}

/// Bytes as `df -h` gives them, rounded up.
pub fn human(bytes: u64) -> String {
    if bytes < 1024 {
        return bytes.to_string();
    }
    let units = ['K', 'M', 'G', 'T', 'P', 'E'];
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match (value * 10.0).ceil() / 10.0 {
        tenths if tenths < 10.0 => format!("{tenths:.1}{}", units[unit]),
        _ => format!("{}{}", value.ceil(), units[unit]),
    }
}

fn ago(now: u64, when: u64) -> String {
    match now.saturating_sub(when) / DAY {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_the_sweep_has_always_printed_them() {
        assert_eq!(size(512), "512K");
        assert_eq!(size(2048), "2M");
        assert_eq!(size(1_572_864), "1.5G");
        assert_eq!(human(10 * 1024 * 1024 * 1024 + 1), "11G");
        assert_eq!(human(9 * 1024 * 1024 * 1024 + 1), "9.1G");
        assert_eq!(ago(DAY * 3, 0), "3 days ago");
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
