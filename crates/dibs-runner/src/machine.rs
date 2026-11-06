use crate::settings::{home, var};
use std::{
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
    path::{Path, PathBuf},
};

/// Where this machine keeps the lock, its timings, its log and its scratch.
#[derive(Debug, Clone)]
pub struct Site {
    pub lock_dir: PathBuf,
    pub history: PathBuf,
    pub log: PathBuf,
    pub scratch: PathBuf,
    /// Its name up to the first dot, as messages give it.
    pub host: String,
    pub scope: Scope,
    /// Where history and log are shared among accounts.
    pub shared_state: PathBuf,
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

/// No lock can be taken, so nothing runs.
#[derive(Debug)]
pub enum NoLock {
    /// The lock directory cannot be written.
    Unwritable(PathBuf),
    /// The machine has a lock every account shares, and this one cannot join it.
    Unjoinable(PathBuf),
}

impl Site {
    /// Finds and checks the machine's directories, as every call does before anything else.
    pub fn set_up() -> Result<Site, NoLock> {
        let shared_state =
            PathBuf::from(var("DIBS_SHARED_STATE_DIR").unwrap_or_else(|| "/var/lib/dibs".into()));
        let LockPlace {
            dir: lock_dir,
            scope,
        } = LockPlace::find(&shared_state)?;
        let probe = lock_dir.join(format!(".writable.{}", std::process::id()));
        if fs::write(&probe, "").is_err() {
            return Err(NoLock::Unwritable(lock_dir));
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
        let (history, log) = match (var("DIBS_HISTORY"), var("DIBS_LOG")) {
            (None, None) if shared_state.writable_dir() => {
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
        Ok(Site {
            lock_dir,
            history,
            log,
            scratch,
            host: short_hostname(None),
            scope,
            shared_state,
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
    /// The tmpfs directory machines set up before the one in the shared state, still used where
    /// it is there so that runners of either age meet in it.
    fn tmpfs() -> PathBuf {
        PathBuf::from(var("DIBS_SHARED_LOCK_DIR").unwrap_or_else(|| "/dev/shm/dibs-lock".into()))
    }

    /// `DIBS_LOCK_DIR`, else a shared directory, else this account's own. A shared directory
    /// that is there and cannot be joined is refused: falling back would tell this account the
    /// machine is idle while another's benchmark runs.
    fn find(shared_state: &Path) -> Result<LockPlace, NoLock> {
        if let Some(dir) = var("DIBS_LOCK_DIR") {
            let dir = PathBuf::from(dir);
            let _ = fs::create_dir_all(&dir);
            return Ok(LockPlace {
                dir,
                scope: Scope::Explicit,
            });
        }
        let tmpfs = LockPlace::tmpfs();
        if tmpfs.is_dir() {
            return LockPlace::joined(tmpfs);
        }
        if shared_state.is_dir() {
            let dir = shared_state.join("lock");
            if !dir.is_dir() && fs::create_dir(&dir).is_ok() {
                // Made under this account's umask, so opened up to the group the parent gives it.
                let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o2775));
            }
            return LockPlace::joined(dir);
        }
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        let dir =
            PathBuf::from(var("XDG_RUNTIME_DIR").unwrap_or_else(|| format!("/run/user/{uid}")))
                .join("dibs-lock");
        let dir = match fs::create_dir_all(&dir) {
            Ok(()) => dir,
            Err(_) => {
                let dir = PathBuf::from(format!("/tmp/dibs-lock-{uid}"));
                let _ = fs::create_dir_all(&dir);
                dir
            }
        };
        Ok(LockPlace {
            dir,
            scope: Scope::PerUser,
        })
    }

    fn joined(dir: PathBuf) -> Result<LockPlace, NoLock> {
        match dir.writable_dir() {
            true => Ok(LockPlace {
                dir,
                scope: Scope::Shared,
            }),
            false => Err(NoLock::Unjoinable(dir)),
        }
    }
}

impl NoLock {
    pub fn said(&self) -> String {
        match self {
            NoLock::Unwritable(dir) => format!(
                "dibs: {} cannot be written, so no lock can be taken. Nothing was run.\n  \
                 A sandboxed shell is the usual cause: it sees the lock directory and cannot\n  \
                 write in it. Unlocked work beside a measurement is the one outcome this exists\n  \
                 to prevent, and another directory would not exclude the sessions using this one.\n",
                dir.display()
            ),
            NoLock::Unjoinable(dir) => format!(
                "dibs: {} is the lock every account on this machine shares, and this one cannot\n  \
                 write in it. Nothing was run. A lock of its own would tell it the machine is idle\n  \
                 while another account measures. Ask whoever runs the machine to add this account\n  \
                 to the directory's group, then log in again.\n",
                dir.display()
            ),
        }
    }
}
/// A directory this account may write in.
pub trait WritableDir {
    fn writable_dir(&self) -> bool;
}

impl WritableDir for Path {
    fn writable_dir(&self) -> bool {
        let Ok(path) = CString::new(self.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: access only reads the path.
        self.is_dir() && unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0
    }
}

/// A computer's name up to its first dot, as `hostname -s` prints it: the one `given`, or this
/// computer's own.
pub fn short_hostname(given: Option<String>) -> String {
    let full = given.unwrap_or_else(|| {
        let mut name = [0u8; 256];
        // SAFETY: gethostname writes at most the buffer's length.
        if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } != 0 {
            return String::new();
        }
        let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
        String::from_utf8_lossy(&name[..end]).into_owned()
    });
    full.split('.').next().unwrap_or_default().to_string()
}

/// The account's name, or its number where it has none.
pub fn account_name(uid: u32) -> String {
    let mut buffer = vec![0 as libc::c_char; 4096];
    // SAFETY: passwd is plain data; getpwuid_r fills it and the buffer, both owned here.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let read = unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if read != 0 || found.is_null() {
        return uid.to_string();
    }
    // SAFETY: getpwuid_r left pw_name pointing at a NUL-terminated name in the buffer.
    unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }
        .to_string_lossy()
        .into_owned()
}

/// This process's account.
pub fn own_account() -> String {
    // SAFETY: getuid cannot fail.
    account_name(unsafe { libc::getuid() })
}
