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

/// The lock files, open. Everyone passes through the gate, and an exclusive waiter keeps
/// holding it while it waits for `rw`, so a trickle of shared jobs cannot starve a benchmark.
/// A preemptible job passes no gate and holds `ahead` beside `rw`; a benchmark takes `ahead`
/// exclusive once it has `rw`, which each such job queued after it lets go of while frozen.
pub struct Lock {
    gate: File,
    rw: File,
    ahead: File,
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
            ahead: Lock::file(&dir.ahead())?,
        })
    }

    fn file(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
    }

    /// Whether nothing holds `rw` or `ahead` this moment, asked without passing the gate.
    pub fn untaken(dir: &LockDir) -> bool {
        [dir.rw(), dir.ahead()].iter().all(|path| {
            Lock::file(path).is_ok_and(|file| {
                let free = file.try_lock().is_ok();
                let _ = file.unlock();
                free
            })
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

    /// Takes `ahead`, giving up at the deadline where there is one: false then.
    pub fn take_ahead(&self, hold: Hold, deadline: Option<Instant>) -> io::Result<bool> {
        Lock::wait(&self.ahead, hold, deadline)
    }

    /// Whether `ahead` could be taken shared this moment; it is let go again at once.
    pub fn free_ahead(&self) -> bool {
        let free = self.ahead.try_lock_shared().is_ok();
        if free {
            let _ = self.ahead.unlock();
        }
        free
    }

    pub fn let_ahead_go(&self) {
        let _ = self.ahead.unlock();
    }

    /// `ahead` and `rw` as second handles on the same locks, for the thread that freezes the
    /// job: what it lets go of, this lets go of too.
    pub fn handles(&self) -> io::Result<(File, File)> {
        Ok((self.ahead.try_clone()?, self.rw.try_clone()?))
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

    pub fn try_hold(file: &File, hold: Hold) -> io::Result<bool> {
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
