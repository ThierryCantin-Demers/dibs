use crate::{
    platform::{Host, Platform as _},
    tree::{clocks::Dates as _, spread::Spread as _},
};
use std::{
    collections::{HashMap, hash_map},
    ffi::CString,
    fs, io,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{FileTypeExt as _, MetadataExt as _, symlink},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
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
    reflinks: Reflinks,
}

impl Copier {
    pub fn new(reflinks: Reflinks) -> Self {
        Copier { reflinks }
    }

    /// `from` copied to `to`, which must not exist, its files shared out among the cores. What a
    /// failed copy made is left for the caller to remove.
    pub fn tree(&self, from: &Path, to: &Path, sharing: Sharing) -> io::Result<()> {
        let mut laid = Laid::default();
        laid.out(from, to, &mut HashMap::new())?;
        let failed = AtomicBool::new(false);
        let made = laid
            .files
            .spread(|file| match failed.load(Ordering::Relaxed) {
                true => Ok(()),
                false => self
                    .made(file, sharing)
                    .inspect_err(|_| failed.store(true, Ordering::Relaxed)),
            });
        made.into_iter().collect::<io::Result<()>>()?;
        for link in &laid.links {
            fs::hard_link(&link.first, &link.to)?;
        }
        laid.dirs.iter().try_for_each(Entry::dated)
    }

    fn made(&self, entry: &Entry, sharing: Sharing) -> io::Result<()> {
        match entry.meta.is_file() {
            true => self.file(&entry.from, &entry.to, sharing)?,
            false => special(&entry.to, &entry.meta)?,
        }
        entry.dated()
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

/// A tree's copy as one walk lays it out: its directories and symlinks made as it goes, and what
/// is left to make once they are there.
#[derive(Default)]
struct Laid {
    files: Vec<Entry>,
    /// A file's second and later names, made once its first is copied.
    links: Vec<Link>,
    /// Dated last, deepest first, since writing into a directory moves its time.
    dirs: Vec<Entry>,
}

struct Entry {
    from: PathBuf,
    to: PathBuf,
    meta: fs::Metadata,
}

struct Link {
    first: PathBuf,
    to: PathBuf,
}

impl Laid {
    fn out(
        &mut self,
        from: &Path,
        to: &Path,
        linked: &mut HashMap<(u64, u64), PathBuf>,
    ) -> io::Result<()> {
        let meta = fs::symlink_metadata(from)?;
        if meta.file_type().is_symlink() {
            return symlink(fs::read_link(from)?, to);
        }
        let entry = Entry {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
            meta,
        };
        if entry.meta.is_dir() {
            fs::create_dir(to)?;
            for child in fs::read_dir(from)? {
                let child = child?;
                self.out(&child.path(), &to.join(child.file_name()), linked)?;
            }
            self.dirs.push(entry);
            return Ok(());
        }
        if entry.meta.nlink() > 1 {
            match linked.entry((entry.meta.dev(), entry.meta.ino())) {
                hash_map::Entry::Occupied(first) => {
                    self.links.push(Link {
                        first: first.get().clone(),
                        to: entry.to,
                    });
                    return Ok(());
                }
                hash_map::Entry::Vacant(first) => {
                    first.insert(entry.to.clone());
                }
            }
        }
        self.files.push(entry);
        Ok(())
    }
}

impl Entry {
    fn dated(&self) -> io::Result<()> {
        fs::set_permissions(&self.to, self.meta.permissions())?;
        self.to.date_like(&self.meta)
    }
}

/// A FIFO, socket or device made anew, as `cp -a` makes one: opening a FIFO to read it waits for
/// a writer that may never come. macOS lets only root mknod, so a FIFO is made by mkfifo and a
/// socket by binding one, which leaves it once nothing listens.
#[allow(
    clippy::unnecessary_cast,
    reason = "mode_t and dev_t are narrower on macOS"
)]
fn special(to: &Path, of: &fs::Metadata) -> io::Result<()> {
    if of.file_type().is_socket() {
        return UnixListener::bind(to).map(drop);
    }
    let path = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: mkfifo and mknod read a NUL-terminated path, alive here.
    let made = unsafe {
        match of.file_type().is_fifo() {
            true => libc::mkfifo(path.as_ptr(), of.mode() as libc::mode_t & 0o7777),
            false => libc::mknod(
                path.as_ptr(),
                of.mode() as libc::mode_t,
                of.rdev() as libc::dev_t,
            ),
        }
    };
    match made {
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
        self.0.touch_existing()?;
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
