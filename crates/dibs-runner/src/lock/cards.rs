use crate::lock::{Hold, Lock, LockDir};
use std::{
    fs::{self, File},
    io,
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::PermissionsExt as _,
    },
};

/// The cards a benchmark runs on, each held exclusive.
pub struct Cards {
    held: Vec<File>,
}

impl Cards {
    /// Every card at once, or none: a set held in part would keep one card while it waits on
    /// another, which two benchmarks asking in opposite orders turn into a deadlock.
    pub fn take_now(dir: &LockDir, pcis: &[String]) -> io::Result<Option<Cards>> {
        let mut pcis = pcis.to_vec();
        pcis.sort();
        pcis.dedup();
        let mut held = Vec::new();
        for pci in &pcis {
            let path = dir.card_lock(pci);
            if let Some(parent) = path.parent()
                && !parent.is_dir()
                && fs::create_dir(parent).is_ok()
            {
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o2775));
            }
            let file = Lock::file(&path)?;
            if !Lock::try_hold(&file, Hold::Exclusive)? {
                return Ok(None);
            }
            held.push(file);
        }
        Ok(Some(Cards { held }))
    }

    /// A flock lasts as long as any copy of its descriptor, so a job given these keeps its cards
    /// while it winds down after its runner died.
    pub fn descriptors(&self) -> impl Iterator<Item = RawFd> + '_ {
        self.held.iter().map(AsRawFd::as_raw_fd)
    }
}
