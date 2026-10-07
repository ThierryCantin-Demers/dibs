use crate::{
    clock::Span,
    tree::git::{Commands, Git},
};
use std::{
    ffi::CString,
    fs::{self, FileTimes},
    io,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};

/// The marker a prepare leaves, whose time is when a tree or a cache was last used.
pub const USED: &str = ".dibs-used";

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
    /// Whole days from `when` to `now`.
    pub fn days(now: u64, when: u64) -> u64 {
        now.saturating_sub(when) / Span::DAY.0
    }
}

/// A path's file times: read as when it was last used or written, and set as `touch` sets them.
pub trait Dates {
    /// When a tree or a cache was last used: its marker's time, else its own.
    fn used(&self, now: u64) -> u64;
    /// When a job's directory or a leftover was last written.
    fn written(&self, now: u64) -> u64;
    /// `find -mtime +days`: unchanged for more than `days` whole days.
    fn unchanged_for(&self, now: u64, days: u64) -> bool;
    /// `touch`: made if missing, and dated now.
    fn touch(&self) -> io::Result<()>;
    /// `touch -c`: an existing file dated now.
    fn touch_existing(&self) -> io::Result<()>;
    /// `touch -r`: given the times `of` holds, which needs no permission to read it.
    fn date_like(&self, of: &fs::Metadata) -> io::Result<()>;
}

impl Dates for Path {
    fn used(&self, now: u64) -> u64 {
        fs::metadata(self.join(USED))
            .or_else(|_| fs::symlink_metadata(self))
            .map_or(now, |m| m.mtime().max(0) as u64)
    }

    fn written(&self, now: u64) -> u64 {
        fs::symlink_metadata(self).map_or(now, |m| m.mtime().max(0) as u64)
    }

    fn unchanged_for(&self, now: u64, days: u64) -> bool {
        fs::metadata(self).is_ok_and(|m| Clocks::days(now, m.mtime().max(0) as u64) > days)
    }

    fn touch(&self) -> io::Result<()> {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self)?;
        let now = SystemTime::now();
        file.set_times(FileTimes::new().set_accessed(now).set_modified(now))
    }

    fn touch_existing(&self) -> io::Result<()> {
        let path = CString::new(self.as_os_str().as_bytes())?;
        // SAFETY: utimensat reads a NUL-terminated path; null times mean now.
        match unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), std::ptr::null(), 0) } {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }

    fn date_like(&self, of: &fs::Metadata) -> io::Result<()> {
        let path = CString::new(self.as_os_str().as_bytes())?;
        let times = [
            libc::timespec {
                tv_sec: of.atime(),
                tv_nsec: of.atime_nsec(),
            },
            libc::timespec {
                tv_sec: of.mtime(),
                tv_nsec: of.mtime_nsec(),
            },
        ];
        // SAFETY: utimensat reads a NUL-terminated path and two timespecs, both alive here.
        match unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) } {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

/// Removing what a sweep collects: a worktree is git's to remove, and one git has lost is a
/// plain directory. What cannot all go is said and left for the next sweep, which a prepare must
/// never fail over.
pub struct Removal<'a> {
    commands: Option<&'a Commands<'a>>,
    say: &'a dyn Fn(&str),
}

impl<'a> Removal<'a> {
    /// Commands run through `commands` when given, so a prepare's cap and stop reach them.
    pub fn new(commands: Option<&'a Commands<'a>>, say: &'a dyn Fn(&str)) -> Self {
        Removal { commands, say }
    }

    pub fn tree(&self, tree: &Path) -> bool {
        let mut git = Git(tree).command();
        git.args(["worktree", "remove", "--force"]).arg(tree);
        self.run(git) || self.path(tree)
    }

    /// What git keeps of the worktrees removed from a clone, cleared.
    pub fn prune(&self, clone: &Path) {
        let mut git = Git(clone).command();
        git.args(["worktree", "prune"]);
        self.run(git);
    }

    pub fn path(&self, path: &Path) -> bool {
        let gone = path.remove_all();
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

/// What a path holds: listed, emptied or removed.
pub trait Contents {
    /// Its entries, hidden ones included, sorted as the shell's glob lists them.
    fn entries(&self) -> Vec<PathBuf>;
    /// Every regular file below it.
    fn files(&self) -> Vec<PathBuf>;
    /// `: >`: emptied, or made, and dated now.
    fn make_empty(&self) -> io::Result<()>;
    /// `rm -rf`: everything that can go goes, and whether all of it did.
    fn remove_all(&self) -> bool;
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

    fn make_empty(&self) -> io::Result<()> {
        fs::File::create(self)?;
        self.touch()
    }

    fn remove_all(&self) -> bool {
        let Ok(meta) = fs::symlink_metadata(self) else {
            return true;
        };
        if !meta.is_dir() {
            return fs::remove_file(self).is_ok();
        }
        let entries: Vec<PathBuf> = fs::read_dir(self)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        let mut all = true;
        for entry in entries {
            all &= entry.remove_all();
        }
        all && fs::remove_dir(self).is_ok()
    }
}
