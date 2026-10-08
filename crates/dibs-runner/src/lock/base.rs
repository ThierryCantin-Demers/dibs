use crate::lock::records::LockDir;
use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    path::Path,
    thread,
    time::{Duration, Instant},
};

/// How often a wait with a deadline tries the lock again.
const RETRY: Duration = Duration::from_millis(25);

/// The two lock files, open. Everyone passes through the gate, and an exclusive waiter keeps
/// holding it while it waits for `rw`, so a trickle of shared jobs cannot starve a benchmark.
pub struct Lock {
    gate: File,
    rw: File,
}

/// How a job holds `rw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    Shared,
    Exclusive,
}

impl Lock {
    /// Opened close-on-exec, so nothing a job starts can keep the lock after the job.
    pub fn open(dir: &LockDir) -> io::Result<Lock> {
        Ok(Lock {
            gate: Lock::file(&dir.gate())?,
            rw: Lock::file(&dir.rw())?,
        })
    }

    fn file(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
    }

    /// Whether nothing holds `rw` this moment, asked without passing the gate.
    pub fn untaken(dir: &LockDir) -> bool {
        Lock::file(&dir.rw()).is_ok_and(|rw| {
            let free = rw.try_lock().is_ok();
            let _ = rw.unlock();
            free
        })
    }

    /// Passes the gate, giving up at the deadline where there is one: false then.
    pub fn pass_gate(&self, deadline: Option<Instant>) -> io::Result<bool> {
        Lock::wait(&self.gate, Hold::Exclusive, deadline)
    }

    pub fn leave_gate(&self) {
        let _ = self.gate.unlock();
    }

    /// Whether `rw` could be taken this moment; it is let go again at once.
    pub fn free(&self, hold: Hold) -> bool {
        let taken = match hold {
            Hold::Shared => self.rw.try_lock_shared(),
            Hold::Exclusive => self.rw.try_lock(),
        };
        let free = taken.is_ok();
        if free {
            let _ = self.rw.unlock();
        }
        free
    }

    /// Takes `rw`, giving up at the deadline where there is one: false then.
    pub fn take(&self, hold: Hold, deadline: Option<Instant>) -> io::Result<bool> {
        Lock::wait(&self.rw, hold, deadline)
    }

    /// Blocks in the kernel with no deadline. With one, it tries again until then, since a
    /// blocked flock can only be cut short by a signal arriving at the right moment.
    fn wait(file: &File, hold: Hold, deadline: Option<Instant>) -> io::Result<bool> {
        let Some(deadline) = deadline else {
            match hold {
                Hold::Shared => file.lock_shared()?,
                Hold::Exclusive => file.lock()?,
            }
            return Ok(true);
        };
        let mut taken = Lock::try_hold(file, hold);
        while matches!(taken, Ok(false)) && Instant::now() < deadline {
            thread::sleep(RETRY.min(deadline.saturating_duration_since(Instant::now())));
            taken = Lock::try_hold(file, hold);
        }
        taken
    }

    fn try_hold(file: &File, hold: Hold) -> io::Result<bool> {
        let tried = match hold {
            Hold::Shared => file.try_lock_shared(),
            Hold::Exclusive => file.try_lock(),
        };
        match tried {
            Ok(()) => Ok(true),
            Err(TryLockError::WouldBlock) => Ok(false),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }
}
