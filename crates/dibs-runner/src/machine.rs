use crate::settings::{home, var};
use std::{
    ffi::CString,
    fs, io,
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
};

/// Where this machine keeps the lock, its timings, its log and its scratch.
#[derive(Debug, Clone)]
pub struct Machine {
    pub lock_dir: PathBuf,
    pub history: PathBuf,
    pub log: PathBuf,
    pub scratch: PathBuf,
    /// Its name up to the first dot, as messages give it.
    pub host: String,
}

/// Whose lock directory it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `DIBS_LOCK_DIR`.
    Explicit,
    /// The directory every account on the machine meets in.
    Shared,
    /// This account's own, which excludes nobody else's jobs.
    PerUser,
}

/// The lock directory cannot be written, so no lock can be taken.
#[derive(Debug)]
pub struct Unwritable(pub PathBuf);

impl Machine {
    /// Finds and checks the machine's directories, as every call does before anything else.
    pub fn set_up() -> Result<Machine, Unwritable> {
        let LockPlace {
            dir: lock_dir,
            scope,
        } = LockPlace::find();
        let probe = lock_dir.join(format!(".writable.{}", std::process::id()));
        if fs::write(&probe, "").is_err() {
            return Err(Unwritable(lock_dir));
        }
        let _ = fs::remove_file(&probe);
        if scope == Scope::Shared {
            // SAFETY: umask only sets this process's file mode mask.
            unsafe { libc::umask(0o002) };
        }
        let state = var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/state"))
            .join("dibs");
        let shared_state =
            PathBuf::from(var("DIBS_SHARED_STATE_DIR").unwrap_or_else(|| "/var/lib/dibs".into()));
        let (history, log) = match (var("DIBS_HISTORY"), var("DIBS_LOG")) {
            (None, None) if writable_dir(&shared_state) => {
                (shared_state.join("history"), shared_state.join("log"))
            }
            (history, log) => (
                history.map_or_else(|| state.join("history"), PathBuf::from),
                log.map_or_else(|| state.join("log"), PathBuf::from),
            ),
        };
        for file in [&history, &log] {
            if let Some(dir) = file.parent() {
                let _ = fs::create_dir_all(dir);
            }
        }
        let scratch = var("DIBS_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".cache/dibs"));
        let _ = fs::create_dir_all(scratch.join("tmp"));
        Ok(Machine {
            lock_dir,
            history,
            log,
            scratch,
            host: short_hostname(),
        })
    }

    /// Scratch for a job's temporary files, which `TMPDIR` points into.
    pub fn tmp(&self) -> PathBuf {
        self.scratch.join("tmp")
    }

    pub fn jobs(&self) -> PathBuf {
        self.scratch.join("jobs")
    }
}

/// Where the lock directory is, and whose it is.
struct LockPlace {
    dir: PathBuf,
    scope: Scope,
}

impl LockPlace {
    /// The first of `DIBS_LOCK_DIR`, the shared directory when it can be written, the runtime
    /// directory, then `/tmp`.
    fn find() -> LockPlace {
        let shared = PathBuf::from(
            var("DIBS_SHARED_LOCK_DIR").unwrap_or_else(|| "/dev/shm/dibs-lock".into()),
        );
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        let (dir, scope) = match var("DIBS_LOCK_DIR") {
            Some(dir) => (PathBuf::from(dir), Scope::Explicit),
            None if writable_dir(&shared) => (shared, Scope::Shared),
            None => (
                PathBuf::from(var("XDG_RUNTIME_DIR").unwrap_or_else(|| format!("/run/user/{uid}")))
                    .join("dibs-lock"),
                Scope::PerUser,
            ),
        };
        match fs::create_dir_all(&dir) {
            Ok(()) => LockPlace { dir, scope },
            Err(_) => {
                let dir = PathBuf::from(format!("/tmp/dibs-lock-{uid}"));
                let _ = fs::create_dir_all(&dir);
                LockPlace {
                    dir,
                    scope: Scope::PerUser,
                }
            }
        }
    }
}

impl Unwritable {
    pub fn said(&self) -> String {
        format!(
            "dibs: {} cannot be written, so no lock can be taken. Nothing was run.\n  \
             A sandboxed shell is the usual cause: it sees the lock directory and cannot\n  \
             write in it. Unlocked work beside a measurement is the one outcome this exists\n  \
             to prevent, and another directory would not exclude the sessions using this one.\n",
            self.0.display()
        )
    }
}

fn writable_dir(dir: &Path) -> bool {
    let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: access only reads the path.
    dir.is_dir() && unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0
}

/// `hostname -s`.
pub fn short_hostname() -> String {
    let mut name = [0u8; 256];
    // SAFETY: gethostname writes at most the buffer's length.
    if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } != 0 {
        return String::new();
    }
    let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
    let full = String::from_utf8_lossy(&name[..end]).into_owned();
    full.split('.').next().unwrap_or_default().to_string()
}

/// Lines in a file, as `wc -l` counts them.
pub fn line_count(path: &Path) -> io::Result<usize> {
    Ok(fs::read(path)?.iter().filter(|b| **b == b'\n').count())
}
