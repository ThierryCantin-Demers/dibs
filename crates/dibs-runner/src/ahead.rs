//! Jobs that only compile, started ahead of a benchmark queued before them and frozen while it
//! runs, so the machine is not left to one long shared job while a benchmark waits on it.

use crate::{
    lock::{Hold, Lock, LockDir},
    settings::{home, var},
    stop::{Stage, Stopper},
};
use dibs_format::{JobId, status::Ahead};
use std::{
    fs::{File, TryLockError},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// How often a job started ahead looks for a benchmark asking it to freeze, and for the end of
/// one that did.
const POLL: Duration = Duration::from_millis(100);

/// How long a job has spent frozen, which its `--max` does not count.
#[derive(Debug, Default)]
pub struct Frozen {
    /// Since when, while it is, and the time of the freezes before.
    state: Mutex<(Option<Instant>, Duration)>,
}

impl Frozen {
    fn freeze(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0.get_or_insert_with(Instant::now);
    }

    fn thaw(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(since) = state.0.take() {
            state.1 += since.elapsed();
        }
    }

    pub fn total(&self) -> Duration {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.1 + state.0.map_or(Duration::ZERO, |since| since.elapsed())
    }
}

/// What the thread that freezes a job needs: where benchmarks ask, when the job arrived, its
/// handle on `ahead`, and the call whose job it freezes.
pub struct Watch {
    pub dir: LockDir,
    /// The runner's, which its records are named by.
    pub pid: u32,
    /// In nanoseconds, as the benchmarks asking say theirs.
    pub arrived: u128,
    pub ahead: File,
    pub stopper: Arc<Stopper>,
    pub frozen: Arc<Frozen>,
}

/// Freezes the job whenever a benchmark that arrived no later than it asks, and lets it go once
/// that benchmark has run.
pub struct Preemption {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Preemption {
    pub fn start(watch: Watch) -> Preemption {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let cargo = var("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".cargo"));
        let thread = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if let Some(bench) = watch.dir.pausing_before(watch.arrived) {
                    watch.pause(&bench, &cargo, &stopped);
                }
                thread::sleep(POLL);
            }
        });
        Preemption { stop, thread }
    }

    /// Ends the thread, which may be waiting with the job frozen when the job was stopped.
    pub fn end(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

impl Watch {
    /// Freezes the job's group and lets go of `ahead` for the benchmark. A job still laying out
    /// its tree, or already ended, is asked again on the next look.
    fn pause(&self, bench: &JobId, cargo: &Path, stop: &AtomicBool) {
        let state = self.stopper.state();
        let Stage::Running(job) = state.stage else {
            return;
        };
        if cache_taken(cargo) {
            return;
        }
        signal_group(job, libc::SIGSTOP);
        // It may have taken the cache's lock between the look and the freeze.
        if cache_taken(cargo) {
            signal_group(job, libc::SIGCONT);
            return;
        }
        drop(state);
        self.frozen.freeze();
        self.dir.mark_ahead(self.pid, &Ahead::Paused(bench.clone()));
        let _ = self.ahead.unlock();
        while !self.resumed() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(POLL);
        }
        signal_group(job, libc::SIGCONT);
        self.frozen.thaw();
        self.dir.mark_ahead(self.pid, &Ahead::Running);
    }

    /// `ahead` taken again, once no benchmark queued before the job asks and the one that ran has
    /// let it go.
    fn resumed(&self) -> bool {
        if self.dir.pausing_before(self.arrived).is_some() {
            return false;
        }
        if !matches!(Lock::try_hold(&self.ahead, Hold::Shared), Ok(true)) {
            return false;
        }
        if self.dir.pausing_before(self.arrived).is_some() {
            let _ = self.ahead.unlock();
            return false;
        }
        true
    }
}

/// Cargo's lock on the package cache, held for a download, which a frozen holder would keep from
/// the benchmark's own cargo for as long as it runs.
fn cache_taken(cargo: &Path) -> bool {
    File::open(cargo.join(".package-cache"))
        .is_ok_and(|cache| matches!(cache.try_lock_shared(), Err(TryLockError::WouldBlock)))
}

fn signal_group(job: u32, signal: libc::c_int) {
    // SAFETY: kill only sends a signal, here to the job's own group.
    unsafe { libc::kill(-(job as libc::pid_t), signal) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_time_counts_only_while_frozen() {
        let frozen = Frozen::default();
        assert_eq!(frozen.total(), Duration::ZERO);
        frozen.freeze();
        thread::sleep(Duration::from_millis(20));
        frozen.thaw();
        let once = frozen.total();
        assert!(once >= Duration::from_millis(20));
        thread::sleep(Duration::from_millis(20));
        assert_eq!(frozen.total(), once, "thawed, it stops counting");
    }

    #[test]
    fn a_cache_held_for_a_download_is_taken_and_one_read_beside_is_not() {
        let cargo = std::env::temp_dir().join(format!("dibs-cache-{}", std::process::id()));
        std::fs::create_dir_all(&cargo).unwrap();
        assert!(!cache_taken(&cargo), "no cache file at all");
        let cache = File::create(cargo.join(".package-cache")).unwrap();
        assert!(!cache_taken(&cargo));
        cache.lock_shared().unwrap();
        assert!(!cache_taken(&cargo), "another reader");
        cache.unlock().unwrap();
        cache.lock().unwrap();
        assert!(cache_taken(&cargo), "a download");
        drop(cache);
        let _ = std::fs::remove_dir_all(&cargo);
    }
}
