//! Shared jobs stopped while another call's urgent job has the machine, and run again after it.

use crate::{
    lock::LockDir,
    platform::{Host, Platform as _},
    stop::{Stage, Stopper},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// How often a running job looks for an urgent job, and a parked one for its end.
pub const POLL: Duration = Duration::from_millis(100);

/// Stops the job's tree once another call's urgent job claims the machine.
pub struct Parking {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<bool>,
}

impl Parking {
    /// `pid` is the runner's, which its records are named by.
    pub fn start(dir: LockDir, pid: u32, stopper: Arc<Stopper>) -> Parking {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if dir.urgent_besides(pid).is_some() {
                    let mut state = stopper.state();
                    let Stage::Running(job) = state.stage else {
                        return false;
                    };
                    Host::reap(&[job]);
                    state.stage = Stage::Parked;
                    return true;
                }
                thread::sleep(POLL);
            }
            false
        });
        Parking { stop, thread }
    }

    /// Whether it parked the job.
    pub fn end(self) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().unwrap_or(false)
    }
}
