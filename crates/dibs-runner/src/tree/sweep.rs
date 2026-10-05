use crate::tree::{
    builds::Builds,
    copy::{remove_all, touch},
    git::Commands,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

const DAY: u64 = 86400;

/// What every prepare collects, of every repo rather than only the one being prepared: most runs
/// are local, and a repo nobody prepares any more would otherwise never be swept at all.
///
/// A target directory goes on a short clock because the disk is what runs out first on a
/// machine, and a compilation cache makes refilling one cheap. One with no marker predates the
/// marker, so it is dated rather than deleted. The current tree and target were touched a moment
/// ago, and a running job touched its own when it started, so neither can be a victim. A sweep is
/// someone else's housekeeping, and must never be why a tree did not arrive: what it cannot remove
/// is said and left for the next.
pub struct Sweep<'a> {
    pub scratch: &'a Path,
    pub keep_days: u64,
    pub target_keep_days: u64,
    /// The tree and target this prepare made.
    pub worktree: &'a Path,
    pub target: &'a Path,
    pub commands: &'a Commands<'a>,
    pub say: &'a dyn Fn(&str),
}

impl Sweep<'_> {
    pub fn run(&self) {
        for repo in listed(&self.scratch.join("ws")) {
            for old in listed(&repo) {
                if old.is_dir() && old != self.worktree {
                    self.tree(&old);
                }
            }
        }
        for old in listed(&self.scratch.join("jobs")) {
            if older_than(&old, self.keep_days) {
                remove_all(&old);
            }
        }
        for old in listed(&self.scratch.join("target")) {
            if old.is_dir() && old != self.target {
                self.cache(&old);
            }
        }
    }

    /// A worktree, removed once it has gone unused past the keep.
    fn tree(&self, old: &Path) {
        let used = old.join(".dibs-used");
        if !used.exists() {
            let _ = touch(&used);
            return;
        }
        if !older_than(&used, self.keep_days) {
            return;
        }
        let mut git = Command::new("git");
        git.arg("-C")
            .arg(old)
            .args(["worktree", "remove", "--force"])
            .arg(old);
        let removed = self.commands.output(git).is_ok_and(|o| o.status.success());
        if !removed && !remove_all(old) {
            self.left(old);
        }
    }

    /// A target directory: one holding only a sweep's marker was dated while it was deleted and
    /// is no cache, one with no marker is dated, and one unused past its keep goes unless a build
    /// holds it.
    fn cache(&self, old: &Path) {
        let used = old.join(".dibs-used");
        let marker = fs::metadata(&used).ok();
        let only_marker = fs::read_dir(old)
            .map(|d| d.flatten().all(|e| e.file_name() == ".dibs-used"))
            .unwrap_or(false);
        if marker.as_ref().is_some_and(|m| m.is_file() && m.len() > 0) && only_marker {
            let _ = fs::remove_file(&used);
            let _ = fs::remove_dir(old);
            return;
        }
        if marker.is_none() {
            let _ = fs::write(&used, "swept\n");
            return;
        }
        if !older_than(&used, self.target_keep_days) || !(Builds { target: old }).idle() {
            return;
        }
        if !remove_all(old) {
            self.left(old);
        }
    }

    fn left(&self, old: &Path) {
        (self.say)(&format!(
            "dibs: could not remove all of {}; the next sweep tries again\n",
            old.display()
        ));
    }
}

/// A directory's entries, hidden ones aside, as a shell's `*` lists them.
fn listed(dir: &Path) -> Vec<PathBuf> {
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

/// `find -mtime +days`: unchanged for more than `days` whole days.
fn older_than(path: &Path, days: u64) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|at| {
            SystemTime::now()
                .duration_since(at)
                .is_ok_and(|age| age.as_secs() / DAY > days)
        })
}
