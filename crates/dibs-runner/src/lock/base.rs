use crate::lock::records::LockDir;
use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

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

    pub fn pass_gate(&self) -> io::Result<()> {
        self.gate.lock()
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

    pub fn take(&self, hold: Hold) -> io::Result<()> {
        match hold {
            Hold::Shared => self.rw.lock_shared(),
            Hold::Exclusive => self.rw.lock(),
        }
    }
}
