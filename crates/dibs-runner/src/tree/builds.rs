use crate::clock::Deadline;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    time::Duration,
};

/// How often a lock waited for is tried again.
pub const RETRY: Duration = Duration::from_millis(25);

/// The `.cargo-lock` files cargo holds in a target directory while it builds there.
#[derive(Debug, Clone, Copy)]
pub struct Builds<'a> {
    target: &'a Path,
}

/// A build's lock held: shared so that no build starts while a copy is read, or exclusive so that
/// none runs in a target being moved or removed.
pub struct FileLock(#[allow(dead_code, reason = "held for its lock")] File);

impl<'a> Builds<'a> {
    pub fn new(target: &'a Path) -> Self {
        Builds { target }
    }

    /// Its locks, at most three directories down, as `find -maxdepth 3` finds them.
    pub fn locks(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        Builds::below(self.target, 1, &mut found);
        found
    }

    fn below(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name() == ".cargo-lock" {
                found.push(path.clone());
            }
            if depth < 3 && entry.file_type().is_ok_and(|t| t.is_dir()) {
                Builds::below(&path, depth + 1, found);
            }
        }
    }

    /// Whether a build runs there: one of its locks cannot be shared without waiting.
    pub fn running(&self) -> bool {
        self.locks()
            .iter()
            .any(|lock| FileLock::shared_now(lock).is_none())
    }

    /// Every lock taken exclusively, or None when a build holds one.
    pub fn all_exclusive(&self) -> Option<Vec<FileLock>> {
        self.locks()
            .into_iter()
            .map(|lock| {
                let file = File::open(lock).ok()?;
                file.try_lock().ok()?;
                Some(FileLock(file))
            })
            .collect()
    }
}

impl FileLock {
    /// `lock` shared, at once.
    pub fn shared_now(lock: &Path) -> Option<FileLock> {
        let file = File::open(lock).ok()?;
        file.try_lock_shared().ok()?;
        Some(FileLock(file))
    }

    /// `lock` shared, within `wait`.
    pub fn shared_within(lock: &Path, wait: Duration) -> Option<FileLock> {
        let file = File::open(lock).ok()?;
        Deadline::after(Some(wait))
            .until(RETRY, || file.try_lock_shared().is_ok())
            .then_some(FileLock(file))
    }
}
