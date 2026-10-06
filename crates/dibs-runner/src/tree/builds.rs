use crate::clock::Deadline;
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

/// How often a lock waited for is tried again.
const RETRY: Duration = Duration::from_millis(25);

/// The `.cargo-lock` files cargo holds in a target directory while it builds there.
#[derive(Debug, Clone, Copy)]
pub struct Builds<'a> {
    pub target: &'a Path,
}

/// A lock held: a build's, shared so that no build starts while a copy is read, or exclusive so
/// that none runs in a target being moved or removed; or a prepare's, so that two never lay out
/// one tree at once.
pub struct Held(#[allow(dead_code, reason = "held for its lock")] File);

impl Builds<'_> {
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
            .any(|lock| Held::shared_now(lock).is_none())
    }

    /// Every lock taken exclusively, or None when a build holds one.
    pub fn all_exclusive(&self) -> Option<Vec<Held>> {
        self.locks()
            .into_iter()
            .map(|lock| {
                let file = File::open(lock).ok()?;
                file.try_lock().ok()?;
                Some(Held(file))
            })
            .collect()
    }

    /// Whether nothing builds there: every lock could be taken exclusively.
    pub fn idle(&self) -> bool {
        self.all_exclusive().is_some()
    }
}

impl Held {
    /// `lock` shared, at once.
    pub fn shared_now(lock: &Path) -> Option<Held> {
        let file = File::open(lock).ok()?;
        file.try_lock_shared().ok()?;
        Some(Held(file))
    }

    /// `lock` shared, within `wait`.
    pub fn shared_within(lock: &Path, wait: Duration) -> Option<Held> {
        let file = File::open(lock).ok()?;
        Deadline::after(Some(wait))
            .until(RETRY, || file.try_lock_shared().is_ok())
            .then_some(Held(file))
    }

    /// `lock`, made if missing, exclusively at once.
    pub fn exclusive_now(lock: &Path) -> Option<Held> {
        let file = File::create(lock).ok()?;
        file.try_lock().ok()?;
        Some(Held(file))
    }

    /// The lock a prepare holds beside a tree or a target while it decides what to make of it,
    /// and a sweep while it removes it.
    pub fn beside(path: &Path) -> PathBuf {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        path.with_file_name(format!(".{name}.lock"))
    }

    /// `lock`, made if missing, exclusively before `deadline`; None once it has passed.
    pub fn exclusive_by(lock: &Path, deadline: Deadline) -> io::Result<Option<Held>> {
        let file = File::create(lock)?;
        Ok(deadline
            .until(RETRY, || file.try_lock().is_ok())
            .then_some(Held(file)))
    }
}
