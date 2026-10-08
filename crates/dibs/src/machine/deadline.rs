use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// A bound on a call that is asked rather than run: past it, or once its `Stop` is dropped, the
/// call is stopped.
#[derive(Debug, Clone)]
pub struct Deadline {
    at: Option<Instant>,
    shared: Arc<Stopping>,
}

/// Stops the call its deadline bounds when it is dropped.
#[derive(Debug)]
pub struct Stop(Arc<Stopping>);

/// A deadline bounded by its stop alone.
pub struct Stoppable {
    pub deadline: Deadline,
    pub stop: Stop,
}

#[derive(Debug, Default)]
struct Stopping {
    passed: AtomicBool,
    attempt: Mutex<Attempt>,
}

/// The running attempt a stop reaches.
#[derive(Debug, Default)]
enum Attempt {
    #[default]
    Idle,
    Running(Sender<Ended>),
    /// The `Stop` has gone, so an attempt yet to start is stopped as it starts.
    Stopped,
}

/// What an attempt's watcher hears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Reaped,
    Stopped,
}

impl Deadline {
    pub fn after(bound: Duration) -> Deadline {
        Deadline {
            at: Some(Instant::now() + bound),
            shared: Arc::default(),
        }
    }

    /// No bound in time: the call runs until it ends, or until the `Stop` is dropped.
    pub fn stoppable() -> Stoppable {
        let shared = Arc::<Stopping>::default();
        Stoppable {
            deadline: Deadline {
                at: None,
                shared: Arc::clone(&shared),
            },
            stop: Stop(shared),
        }
    }

    /// Bounded in time, rather than by its stop alone.
    pub fn timed(&self) -> bool {
        self.at.is_some()
    }

    pub fn passed(&self) -> bool {
        self.shared.passed.load(Ordering::SeqCst)
    }

    /// Stops the process at the deadline or the stop, unless `ended` says first that it was
    /// reaped. A stop reaches it through `tell`. The watcher ends with what it heard.
    pub fn watch(
        &self,
        pid: u32,
        tell: Sender<Ended>,
        ended: Receiver<Ended>,
    ) -> JoinHandle<Ended> {
        let stopped = {
            let mut attempt = self.shared.lock();
            match *attempt {
                Attempt::Stopped => true,
                _ => {
                    *attempt = Attempt::Running(tell);
                    false
                }
            }
        };
        let left = self
            .at
            .map(|at| at.saturating_duration_since(Instant::now()));
        let shared = Arc::clone(&self.shared);
        thread::spawn(move || {
            let end = match (stopped, left) {
                (true, _) => Ended::Stopped,
                (false, Some(left)) => match ended.recv_timeout(left) {
                    Ok(end) => end,
                    Err(RecvTimeoutError::Timeout) => Ended::Stopped,
                    Err(RecvTimeoutError::Disconnected) => Ended::Reaped,
                },
                (false, None) => ended.recv().unwrap_or(Ended::Reaped),
            };
            let mut attempt = shared.lock();
            if let Attempt::Running(_) = *attempt {
                *attempt = Attempt::Idle;
            }
            drop(attempt);
            if end == Ended::Stopped {
                shared.passed.store(true, Ordering::SeqCst);
                // SAFETY: kill only sends a signal, and `Reaped` follows the reap at once, so the
                // pid is still the call's but for that instant.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            }
            end
        })
    }
}

impl Stopping {
    fn lock(&self) -> MutexGuard<'_, Attempt> {
        self.attempt.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Stop {
    fn drop(&mut self) {
        let mut attempt = self.0.lock();
        if let Attempt::Running(tell) = std::mem::replace(&mut *attempt, Attempt::Stopped) {
            let _ = tell.send(Ended::Stopped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::process::ExitStatusExt as _,
        process::{Child, Command, Stdio},
        sync::mpsc,
    };

    /// A process that runs until its stdin closes or it is signalled. `wait` closes a stdin it
    /// still holds, so a test that means to stop it takes the stdin first.
    fn waiting() -> Child {
        Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("cat starts")
    }

    #[test]
    fn dropping_the_stop_stops_the_call() {
        let Stoppable { deadline, stop } = Deadline::stoppable();
        let mut child = waiting();
        let _open = child.stdin.take();
        let (tell, ended) = mpsc::channel();
        deadline.watch(child.id(), tell, ended);
        drop(stop);
        let status = child.wait().expect("cat is reaped");
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(deadline.passed());
    }

    #[test]
    fn a_stop_dropped_before_the_call_starts_stops_it_as_it_starts() {
        let Stoppable { deadline, stop } = Deadline::stoppable();
        drop(stop);
        let mut child = waiting();
        let _open = child.stdin.take();
        let (tell, ended) = mpsc::channel();
        deadline.watch(child.id(), tell, ended);
        assert_eq!(
            child.wait().expect("cat is reaped").signal(),
            Some(libc::SIGTERM)
        );
    }

    #[test]
    fn a_call_that_ended_is_not_stopped_after() {
        let Stoppable { deadline, stop } = Deadline::stoppable();
        let mut child = waiting();
        let (tell, ended) = mpsc::channel();
        let watcher = deadline.watch(child.id(), tell.clone(), ended);
        drop(child.stdin.take());
        assert!(child.wait().expect("cat is reaped").success());
        tell.send(Ended::Reaped).expect("the watcher listens");
        drop(stop);
        assert_eq!(watcher.join().expect("the watcher ends"), Ended::Reaped);
        assert!(!deadline.passed());
    }
}
