use crate::{
    call::{Call, Journal},
    job::reap,
    lock::LockDir,
    sink::Sink,
};
use dibs_format::{Event, JobId};
use std::{
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, MutexGuard},
    thread,
};

/// What a call has got to, which decides what stopping it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Nothing written on the machine yet.
    Setup,
    /// A peek's command runs, which leaves no records.
    Peeking(u32),
    /// Its records are written and it waits for the lock.
    Queued,
    /// It holds the lock, and its services are starting.
    Starting,
    Running(u32),
    /// Its job has ended.
    Finishing,
}

/// Where a call is, guarded so that a stop and the call's own next step never interleave.
#[derive(Debug)]
pub struct State {
    pub stage: Stage,
    /// Its end is in the log, so stopping it now adds no `aborted`.
    pub logged_end: bool,
    /// Named once it arrives, and on every line it logs from then on.
    pub job: Option<JobId>,
    /// Its `--with` servers, which go with it.
    pub services: Vec<u32>,
}

/// Stops a call from another thread, leaving the machine as the call's own end would.
pub struct Stopper {
    state: Mutex<State>,
    call: Call,
    dir: LockDir,
    log: PathBuf,
    sink: Sink,
}

impl Stopper {
    pub fn new(call: Call, dir: LockDir, log: PathBuf, sink: Sink) -> Stopper {
        Stopper {
            state: Mutex::new(State {
                stage: Stage::Setup,
                logged_end: false,
                job: None,
                services: Vec::new(),
            }),
            call,
            dir,
            log,
            sink,
        }
    }

    /// The call's state, held: nothing stops the call until the guard goes.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// TERM, HUP or INT: whatever runs is stopped, and the call ends with 128 and the signal.
    pub fn signalled(&self, signal: i32) -> ! {
        let mut state = self.state();
        self.abort(&mut state, 128 + signal)
    }

    /// A caller gone while queued takes its call out of the queue; one gone while its job runs
    /// takes the job's tree, and the call finishes as for any other end.
    pub fn caller_gone(&self, why: &str) {
        let mut state = self.state();
        if !matches!(
            state.stage,
            Stage::Queued | Stage::Starting | Stage::Running(_)
        ) {
            return;
        }
        let mut line = self.call.log_line(Event::CallerGone);
        line.command = format!("{why}: {}", self.call.one_line);
        line.job = state.job.clone();
        Journal { path: &self.log }.write(&line);
        match state.stage {
            Stage::Running(work) => reap(&[work]),
            Stage::Starting => reap(&state.services),
            _ => self.abort(&mut state, 128 + libc::SIGTERM),
        }
    }

    fn abort(&self, state: &mut State, code: i32) -> ! {
        let work = match state.stage {
            Stage::Peeking(work) | Stage::Running(work) => Some(work),
            _ => None,
        };
        let stopped: Vec<u32> = work.into_iter().chain(state.services.clone()).collect();
        if !stopped.is_empty() {
            reap(&stopped);
        }
        if matches!(
            state.stage,
            Stage::Queued | Stage::Starting | Stage::Running(_) | Stage::Finishing
        ) {
            self.dir.clear(self.call.pid);
            if !state.logged_end {
                let mut line = self.call.log_line(Event::Aborted);
                line.job = state.job.clone();
                Journal { path: &self.log }.write(&line);
            }
        }
        self.sink.exit_without_waiting(code);
        // SAFETY: _exit ends the process without flushing a stdout another thread may hold.
        unsafe { libc::_exit(code) }
    }
}

/// TERM, HUP and INT, blocked in every thread so that one thread alone takes them.
pub struct Signals {
    set: libc::sigset_t,
}

impl Signals {
    /// Called before any other thread exists, which then inherit the mask. A signal this process
    /// was started ignoring stays ignored: a hold's caller keeps Ctrl-C for its own command.
    pub fn block() -> Signals {
        // SAFETY: the set is initialised by sigemptyset before it is used.
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut set);
            for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
                let mut was: libc::sigaction = std::mem::zeroed();
                libc::sigaction(signal, std::ptr::null(), &mut was);
                if was.sa_sigaction != libc::SIG_IGN {
                    libc::sigaddset(&mut set, signal);
                }
            }
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
        Signals { set }
    }

    /// A child inherits the mask, and a job that cannot be sent TERM cannot be stopped by it.
    pub fn unblocked(command: &mut Command) {
        // SAFETY: the closure makes async-signal-safe calls only.
        unsafe {
            command.pre_exec(|| {
                let mut empty: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut empty);
                libc::pthread_sigmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
                Ok(())
            });
        }
    }

    /// Hands the first of them to the stopper, on a thread of its own.
    pub fn listen(self, stopper: Arc<Stopper>) {
        thread::spawn(move || {
            let mut signal = 0;
            // SAFETY: the set was built by `block`, and sigwait writes one int.
            unsafe { libc::sigwait(&self.set, &mut signal) };
            stopper.signalled(signal)
        });
    }
}
