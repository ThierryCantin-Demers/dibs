use crate::platform::{Host, Platform as _};
use std::{
    collections::HashMap,
    ffi::CString,
    fs::{self, FileTimes},
    io,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, symlink},
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};

/// How this machine shares a file's blocks. A test sets `DIBS_REFLINK` to `copy` or `never` to
/// stand in for a filesystem it does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reflinks {
    Real,
    Copies,
    Never,
}

impl Reflinks {
    pub fn of(setting: Option<&str>) -> Reflinks {
        match setting {
            Some("copy") => Reflinks::Copies,
            Some("never") => Reflinks::Never,
            _ => Reflinks::Real,
        }
    }
}

/// Whether a copy must share its blocks: a target's must, since a full copy per tree would fill
/// the disk, and sources, which are small, may be copied plainly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sharing {
    Required,
    Preferred,
}

/// `cp -a`: modes, times, symlinks and hard links kept.
#[derive(Debug, Clone, Copy)]
pub struct Copier {
    pub reflinks: Reflinks,
}

impl Copier {
    /// `from` copied to `to`, which must not exist. What a failed copy made is left for the
    /// caller to remove.
    pub fn tree(&self, from: &Path, to: &Path, sharing: Sharing) -> io::Result<()> {
        let mut linked = HashMap::new();
        self.entry(from, to, sharing, &mut linked)
    }

    fn entry(
        &self,
        from: &Path,
        to: &Path,
        sharing: Sharing,
        linked: &mut HashMap<(u64, u64), PathBuf>,
    ) -> io::Result<()> {
        let meta = fs::symlink_metadata(from)?;
        if meta.file_type().is_symlink() {
            return symlink(fs::read_link(from)?, to);
        }
        if meta.is_dir() {
            fs::create_dir(to)?;
            for entry in fs::read_dir(from)? {
                let entry = entry?;
                self.entry(&entry.path(), &to.join(entry.file_name()), sharing, linked)?;
            }
        } else {
            let inode = (meta.dev(), meta.ino());
            if meta.nlink() > 1
                && let Some(first) = linked.get(&inode)
            {
                return fs::hard_link(first, to);
            }
            match meta.is_file() {
                true => self.file(from, to, sharing)?,
                false => special(to, &meta)?,
            }
            if meta.nlink() > 1 {
                linked.insert(inode, to.to_path_buf());
            }
        }
        fs::set_permissions(to, meta.permissions())?;
        dated(to, &meta)
    }

    fn file(&self, from: &Path, to: &Path, sharing: Sharing) -> io::Result<()> {
        let shared = match self.reflinks {
            Reflinks::Real => Host::reflink(from, to),
            Reflinks::Copies => fs::copy(from, to).is_ok(),
            Reflinks::Never => false,
        };
        match (shared, sharing) {
            (true, _) => Ok(()),
            (false, Sharing::Preferred) => fs::copy(from, to).map(drop),
            (false, Sharing::Required) => Err(io::Error::other(format!(
                "{} cannot share its blocks here",
                from.display()
            ))),
        }
    }
}

/// A FIFO, socket or device made anew, as `cp -a` makes one: opening a FIFO to read it waits for
/// a writer that may never come.
#[allow(
    clippy::unnecessary_cast,
    reason = "mode_t and dev_t are narrower on macOS"
)]
fn special(to: &Path, of: &fs::Metadata) -> io::Result<()> {
    let path = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: mknod reads a NUL-terminated path, alive here.
    match unsafe {
        libc::mknod(
            path.as_ptr(),
            of.mode() as libc::mode_t,
            of.rdev() as libc::dev_t,
        )
    } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

/// `to` given the times `of` holds, by its path, which needs no permission to read it.
pub fn dated(to: &Path, of: &fs::Metadata) -> io::Result<()> {
    let path = CString::new(to.as_os_str().as_bytes())?;
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

/// `touch -c`: an existing file dated now, by its path.
pub fn now(path: &Path) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: utimensat reads a NUL-terminated path; null times mean now.
    match unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), std::ptr::null(), 0) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

/// Longer than the coarsest tick a file's time is stamped by.
const FILE_TICK: Duration = Duration::from_millis(11);

/// A file whose time marks when something began: whatever is written after it is newer.
pub struct Mark<'a>(pub &'a Path);

impl Mark<'_> {
    /// Dated now.
    pub fn set(&self) -> io::Result<()> {
        now(self.0)?;
        self.passed()
    }

    /// Returns once the clock files are stamped by has moved past it: file times move a tick,
    /// up to 10 ms, at a time.
    pub fn passed(&self) -> io::Result<()> {
        let marked = fs::metadata(self.0)?.modified()?;
        if let Ok(left) = (marked + FILE_TICK).duration_since(SystemTime::now()) {
            thread::sleep(left);
        }
        Ok(())
    }
}

/// `rm -rf`: everything that can go goes, and whether all of it did.
pub fn remove_all(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return true;
    };
    if !meta.is_dir() {
        return fs::remove_file(path).is_ok();
    }
    let entries: Vec<PathBuf> = fs::read_dir(path)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    let mut all = true;
    for entry in entries {
        all &= remove_all(&entry);
    }
    all && fs::remove_dir(path).is_ok()
}

/// `touch`: made if missing, and dated now.
pub fn touch(path: &Path) -> io::Result<()> {
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let now = std::time::SystemTime::now();
    file.set_times(FileTimes::new().set_accessed(now).set_modified(now))
}

/// `: >`: emptied, or made, and dated now.
pub fn empty(path: &Path) -> io::Result<()> {
    fs::File::create(path)?;
    touch(path)
}
