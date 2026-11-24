//! Shared jobs stopped while another call's urgent job has the machine, or, for one run only on
//! an idle machine, while any other job is there, and run again after.

use crate::{
    ahead::Frozen,
    lock::LockDir,
    platform::{Descendants as _, Host, Platform as _},
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

/// Stops the job's tree once another call's urgent job claims the machine, or, for a job run only
/// while the machine is idle, once any other job holds or waits there.
pub struct Parking {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<bool>,
}

impl Parking {
    /// `pid` is the runner's, which its records are named by.
    pub fn start(
        dir: LockDir,
        pid: u32,
        idle: bool,
        stopper: Arc<Stopper>,
        frozen: Arc<Frozen>,
    ) -> Parking {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if dir.urgent_besides(pid).is_some() || (idle && dir.busy_besides(pid).is_some()) {
                    let mut state = stopper.state();
                    let Stage::Running(job) = state.stage else {
                        return false;
                    };
                    // A command that has already ended has run, and is not run twice.
                    if !Host::running(job) {
                        return false;
                    }
                    match frozen.now() {
                        true => kill_frozen(job),
                        false => Host::reap(&[job]),
                    }
                    state.set_stage(Stage::Parked);
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

/// KILL rather than TERM, which would let the tree go to run its handlers beside the benchmark
/// that froze it.
fn kill_frozen(job: u32) {
    let below = Host::processes().below(job);
    // SAFETY: kill only sends a signal, to the job's group and to what it started outside it.
    unsafe {
        libc::kill(-(job as libc::pid_t), libc::SIGKILL);
        for pid in below.into_iter().chain([job]) {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}
