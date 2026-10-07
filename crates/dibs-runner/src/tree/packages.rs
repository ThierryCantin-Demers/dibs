use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// The record a target keeps of every lockfile built into it.
pub const RECORD: &str = ".dibs-packages";
/// A prepare's lockfile, beside its target until a build of it succeeds.
const PENDING: &str = ".dibs-packages.pending.";
/// A staged lockfile whose build never succeeded is gone after this long.
const PENDING_KEPT: Duration = Duration::from_secs(1440 * 60);

/// A lockfile's package lines: short hashes, sorted and each once, as a target's record holds
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lines(BTreeSet<String>);

impl Lines {
    pub fn of(lines: &[String]) -> Lines {
        Lines(lines.iter().cloned().collect())
    }

    /// The lines of a file, None when there is no such file.
    pub fn read(path: &Path) -> Option<Lines> {
        let text = fs::read_to_string(path).ok()?;
        Some(Lines(text.lines().map(str::to_string).collect()))
    }

    pub fn len(&self) -> u64 {
        self.0.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many lines both hold.
    pub fn shared_with(&self, other: &Lines) -> u64 {
        self.0.intersection(&other.0).count() as u64
    }

    pub fn extend(&mut self, other: Lines) {
        self.0.extend(other.0);
    }

    pub fn text(&self) -> String {
        self.0.iter().map(|l| format!("{l}\n")).collect()
    }
}

/// A target directory, as a build cache with a record of what was built into it.
#[derive(Debug, Clone, Copy)]
pub struct Cache<'a> {
    dir: &'a Path,
}

impl<'a> Cache<'a> {
    pub fn new(dir: &'a Path) -> Self {
        Cache { dir }
    }

    pub fn record(&self) -> Option<Lines> {
        Lines::read(&self.dir.join(RECORD))
    }

    /// Whether a prepare marked it used within `span`.
    pub fn used_within(&self, span: Duration) -> bool {
        fs::metadata(self.dir.join(".dibs-used"))
            .and_then(|m| m.modified())
            .is_ok_and(|at| {
                SystemTime::now()
                    .duration_since(at)
                    .map_or(true, |age| age < span)
            })
    }

    /// Its record, and every lockfile staged beside it: what a build running there will have
    /// built once it succeeds.
    pub fn after_its_build(&self) -> Lines {
        let mut all = self.record().unwrap_or_default();
        for staged in self.staged() {
            all.0.extend(Lines::read(&staged).unwrap_or_default().0);
        }
        all
    }

    /// This prepare's lockfile beside the target.
    pub fn stage(&self, token: &str, lines: &Lines) -> io::Result<()> {
        fs::write(self.staged_by(token), lines.text())
    }

    pub fn staged_by(&self, token: &str) -> PathBuf {
        self.dir.join(format!("{PENDING}{token}"))
    }

    /// The staged lockfiles whose builds never succeeded, gone after a day.
    pub fn forget_unbuilt(&self) {
        let now = SystemTime::now();
        for staged in self.staged() {
            let old = fs::metadata(&staged)
                .and_then(|m| m.modified())
                .is_ok_and(|at| now.duration_since(at).unwrap_or_default() > PENDING_KEPT);
            if old {
                let _ = fs::remove_file(staged);
            }
        }
    }

    fn staged(&self) -> Vec<PathBuf> {
        fs::read_dir(self.dir)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(PENDING))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default()
    }
}
