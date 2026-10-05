use crate::tree::{
    builds::Builds,
    copy::{remove_all, touch},
    git::Commands,
};
use std::{
    fs,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    process::Command,
};

const DAY: u64 = 86400;
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
        match past(&used, now, self.keep_days) {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

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
        if !past(&used, now, self.target_keep_days) {
            return Fate::Kept;
        }
        match (Builds { target: cache }).idle() {
            true => Fate::Past,
            false => Fate::Held,
        }
    }

    /// A job's directory, or a leftover file.
    pub fn bulk(&self, path: &Path, now: u64) -> bool {
        past(path, now, self.keep_days)
    }
}

/// When a tree was last used: its marker's time, else its own.
pub fn used(dir: &Path, now: u64) -> u64 {
    fs::metadata(dir.join(USED))
        .or_else(|_| fs::metadata(dir))
        .map_or(now, |m| m.mtime().max(0) as u64)
}

/// `find -mtime +days`: unchanged for more than `days` whole days.
fn past(path: &Path, now: u64, days: u64) -> bool {
    fs::metadata(path).is_ok_and(|m| now.saturating_sub(m.mtime().max(0) as u64) / DAY > days)
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
        let removed = match self.commands {
            Some(commands) => commands.output(git).is_ok_and(|o| o.status.success()),
            None => git.output().is_ok_and(|o| o.status.success()),
        };
        removed || self.path(tree)
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
}

/// A directory's entries, hidden ones aside, sorted as the shell's `*` lists them.
pub fn listed(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}
