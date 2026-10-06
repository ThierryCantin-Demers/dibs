use crate::{
    clock::Span,
    tree::{
        copy::{remove_all, touch},
        git::Commands,
    },
};
use std::{
    fs,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The marker a prepare leaves, whose time is when a tree or a cache was last used.
pub const USED: &str = ".dibs-used";
/// What a sweep writes in a cache it found unmarked; a prepare's marker is empty.
const DATED: &str = "swept\n";

/// What one sweep makes of a tree or a build cache, whether a prepare or `dibs --gc` runs it, so
/// the two never disagree on what goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Used within its keep.
    Kept,
    /// Unmarked, as one made before the marker was: dated now, and judged from there.
    Dated,
    /// Holding only a sweep's marker: dated while it was being removed, and no cache.
    Hollow,
    /// Past its keep, but a build holds it.
    Held,
    /// Past its keep, but a prepare holds it, to revive it or to lay out what it stands in for.
    Preparing,
    /// Past its keep.
    Past,
}

/// How long a tree and a cache are kept unused. A cache goes on a shorter clock because the disk
/// is what runs out first on a machine, and a compilation cache makes refilling one cheap.
#[derive(Debug, Clone, Copy)]
pub struct Clocks {
    pub keep_days: u64,
    pub target_keep_days: u64,
}

impl Clocks {
    pub fn tree(&self, tree: &Path, now: u64) -> Fate {
        let used = tree.join(USED);
        if !used.exists() {
            let _ = touch(&used);
            return Fate::Dated;
        }
        match Clocks::past(&used, now, self.keep_days) {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

    /// By its marker alone: whether a build holds it is for the sweep to ask, holding its locks.
    pub fn cache(&self, cache: &Path, now: u64) -> Fate {
        let used = cache.join(USED);
        let marker = fs::metadata(&used).ok();
        let alone = fs::read_dir(cache)
            .map(|d| d.flatten().all(|e| e.file_name() == USED))
            .unwrap_or(false);
        if marker.as_ref().is_some_and(|m| m.is_file() && m.len() > 0) && alone {
            return Fate::Hollow;
        }
        if marker.is_none() {
            let _ = fs::write(&used, DATED);
            return Fate::Dated;
        }
        match Clocks::past(&used, now, self.target_keep_days) {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

    /// A job's directory, or a leftover file, by its own time.
    pub fn bulk(&self, path: &Path, now: u64) -> Fate {
        let written = fs::symlink_metadata(path).map(|_| Clocks::written(path, now));
        match written.is_ok_and(|at| Clocks::days(now, at) > self.keep_days) {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

    /// When a tree or a cache was last used: its marker's time, else its own.
    pub fn used(dir: &Path, now: u64) -> u64 {
        fs::metadata(dir.join(USED))
            .or_else(|_| fs::symlink_metadata(dir))
            .map_or(now, |m| m.mtime().max(0) as u64)
    }

    /// When a job's directory or a leftover was last written.
    pub fn written(path: &Path, now: u64) -> u64 {
        fs::symlink_metadata(path).map_or(now, |m| m.mtime().max(0) as u64)
    }

    /// Whole days from `when` to `now`.
    pub fn days(now: u64, when: u64) -> u64 {
        now.saturating_sub(when) / Span::DAY.0
    }

    /// `find -mtime +days`: unchanged for more than `days` whole days.
    fn past(path: &Path, now: u64, days: u64) -> bool {
        fs::metadata(path).is_ok_and(|m| Clocks::days(now, m.mtime().max(0) as u64) > days)
    }
}

/// Removing what a sweep collects: a worktree is git's to remove, and one git has lost is a
/// plain directory. What cannot all go is said and left for the next sweep, which a prepare must
/// never fail over.
pub struct Removal<'a> {
    pub commands: Option<&'a Commands<'a>>,
    pub say: &'a dyn Fn(&str),
}

impl Removal<'_> {
    pub fn tree(&self, tree: &Path) -> bool {
        let mut git = Command::new("git");
        git.arg("-C")
            .arg(tree)
            .args(["worktree", "remove", "--force"])
            .arg(tree);
        self.run(git) || self.path(tree)
    }

    /// What git keeps of the worktrees removed from a clone, cleared.
    pub fn prune(&self, clone: &Path) {
        let mut git = Command::new("git");
        git.arg("-C").arg(clone).args(["worktree", "prune"]);
        self.run(git);
    }

    pub fn path(&self, path: &Path) -> bool {
        let gone = remove_all(path);
        if !gone {
            (self.say)(&format!(
                "dibs: could not remove all of {}; the next sweep tries again\n",
                path.display()
            ));
        }
        gone
    }

    /// A cache holding only a sweep's marker, removed quietly.
    pub fn hollow(&self, cache: &Path) {
        let _ = fs::remove_file(cache.join(USED));
        let _ = fs::remove_dir(cache);
    }

    fn run(&self, mut command: Command) -> bool {
        let output = match self.commands {
            Some(commands) => commands.output(command),
            None => command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output(),
        };
        output.is_ok_and(|o| o.status.success())
    }
}

/// What a directory holds, as a sweep reads it.
pub trait Contents {
    /// Its entries, hidden ones included, sorted as the shell's glob lists them.
    fn entries(&self) -> Vec<PathBuf>;
    /// Every regular file below it.
    fn files(&self) -> Vec<PathBuf>;
}

impl Contents for Path {
    fn entries(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = fs::read_dir(self)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        found.sort();
        found
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![self.to_path_buf()];
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
}
