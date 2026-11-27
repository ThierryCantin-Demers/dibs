use crate::{
    call::{Journal, Received},
    job::Group,
    lock::LockDir,
    platform::{Descendants as _, Host, Platform as _},
    sink::Sink,
};
use dibs_format::{Event, Exit, JobId, wire::CheckInReport};
use std::{
    fs::{self, File},
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
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
    /// It holds the lock, and its tree is being laid out, by the command named when one runs.
    Preparing(Option<u32>),
    /// It holds the lock, and its services are starting.
    Starting,
    Running(u32),
    /// It holds the lock and runs nothing while another call's urgent job has the machine.
    Parked,
    /// Its job has ended.
    Finishing,
}

/// Where a call is, guarded so that a stop and the call's own next step never interleave.
#[derive(Debug)]
pub struct State {
    /// Set through `set_stage`, which keeps the time parked.
    pub stage: Stage,
    parked: Duration,
    parked_since: Option<Instant>,
    /// Its end is in the log, so stopping it now adds no `aborted`.
    pub logged_end: bool,
    /// Named once it arrives, and on every line it logs from then on.
    pub job: Option<JobId>,
    /// Its `--with` servers, which go with it.
    pub services: Vec<u32>,
    /// Stopped because its machine was closed with `--now`, which its exit says.
    pub closed: bool,
    /// Its call let go of the job at a check-in, or is telling its caller it does, so the caller
    /// going no longer stops it.
    pub handed: bool,
    /// The caller went while being told: it is stopped after all if the telling failed.
    pub gone_while_handing: bool,
    /// Its job's group has been swept to the end, so its id may name someone else's by now.
    pub swept: bool,
}

impl State {
    pub fn set_stage(&mut self, stage: Stage) {
        match (self.stage, stage) {
            (Stage::Parked, Stage::Parked) => {}
            (Stage::Parked, _) => {
                self.parked += self
                    .parked_since
                    .take()
                    .map_or(Duration::ZERO, |s| s.elapsed());
            }
            (_, Stage::Parked) => self.parked_since = Some(Instant::now()),
            _ => {}
        }
        if let Stage::Running(_) = stage {
            self.swept = false;
        }
        self.stage = stage;
    }

    /// All the time the call has spent parked, the park it is in included.
    pub fn parked(&self) -> Duration {
        self.parked + self.parked_since.map_or(Duration::ZERO, |s| s.elapsed())
    }
}

/// Stops a call from another thread, leaving the machine as the call's own end would.
pub struct Stopper {
    state: Mutex<State>,
    call: Received,
    dir: LockDir,
    log: PathBuf,
    sink: Sink,
    /// What was made for the call alone, removed when it is stopped.
    temporary: Vec<PathBuf>,
}

impl Stopper {
    pub fn new(call: Received, dir: LockDir, log: PathBuf, sink: Sink) -> Stopper {
        Stopper {
            state: Mutex::new(State {
                stage: Stage::Setup,
                parked: Duration::ZERO,
                parked_since: None,
                logged_end: false,
                job: None,
                services: Vec::new(),
                closed: false,
                handed: false,
                gone_while_handing: false,
                swept: false,
            }),
            call,
            dir,
            log,
            sink,
            temporary: Vec::new(),
        }
    }

    pub fn with_temporary(self, temporary: Vec<PathBuf>) -> Stopper {
        Stopper { temporary, ..self }
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
        self.stop_for(Event::CallerGone, why, 128 + libc::SIGTERM);
    }

    /// Lets go of the job at its first check-in: the caller is told, and from then on its going
    /// stops nothing. False when the call is already ending, or nothing could be handed over.
    /// The caller is told without the state held, so a stop is never kept waiting on a caller
    /// that stopped reading.
    pub fn hand_over(&self, checked: CheckInReport, text: &str, report: &File) -> bool {
        {
            let mut state = self.state();
            if !matches!(
                state.stage,
                Stage::Starting | Stage::Running(_) | Stage::Parked
            ) || !self.sink.framed()
            {
                return false;
            }
            state.handed = true;
        }
        let told = self.sink.hand_over(checked, text, report).is_ok();
        let mut state = self.state();
        if told {
            let mut line = self.call.log_line(Event::HandedOver);
            line.job = state.job.clone();
            Journal { path: &self.log }.write(&line);
            return true;
        }
        state.handed = false;
        let gone = std::mem::take(&mut state.gone_while_handing);
        drop(state);
        if gone {
            self.caller_gone("caller gone while told of its check-in");
        }
        false
    }

    /// Its machine closed with `--now`: stopped as for a caller gone, ending with exit 79.
    pub fn closed(&self, why: &str) {
        self.state().closed = true;
        self.stop_for(Event::Killed, why, Exit::Closed.as_status());
    }

    fn stop_for(&self, event: Event, why: &str, code: i32) {
        let mut state = self.state();
        if state.handed && event == Event::CallerGone {
            state.gone_while_handing = true;
            return;
        }
        if !matches!(
            state.stage,
            Stage::Queued
                | Stage::Preparing(_)
                | Stage::Starting
                | Stage::Running(_)
                | Stage::Parked
                | Stage::Peeking(_)
        ) {
            return;
        }
        let mut line = self.call.log_line(event);
        line.command = format!("{why}: {}", self.call.one_line);
        line.job = state.job.clone();
        Journal { path: &self.log }.write(&line);
        match state.stage {
            Stage::Running(work) | Stage::Peeking(work) => Host::reap(&[work]),
            Stage::Starting => Host::reap(&Stopper::starting()),
            _ => self.abort(&mut state, code),
        }
    }

    /// While services start, everything this process runs: the services, and the readiness
    /// check beside them, which may wait on a service for as long as it is given.
    fn starting() -> Vec<u32> {
        Host::processes().below(std::process::id())
    }

    fn abort(&self, state: &mut State, code: i32) -> ! {
        let work = match state.stage {
            Stage::Peeking(work) | Stage::Running(work) | Stage::Preparing(Some(work)) => {
                Some(work)
            }
            _ => None,
        };
        let stopped: Vec<u32> = match state.stage {
            Stage::Starting => Stopper::starting(),
            _ => work.into_iter().chain(state.services.clone()).collect(),
        };
        if !stopped.is_empty() {
            Host::reap(&stopped);
        }
        // A job whose command has ended is still sweeping its group, and what that left, or what
        // left the group, holds the cards until it is gone: the lock goes after it, not before.
        if let Some(work) = work {
            if matches!(state.stage, Stage::Running(_)) && !state.swept {
                Group(work).sweep();
            }
            let left = state.job.as_ref().map(left_by).unwrap_or_default();
            if !left.is_empty() {
                Host::reap(&left);
            }
        }
        if matches!(
            state.stage,
            Stage::Queued
                | Stage::Preparing(_)
                | Stage::Starting
                | Stage::Running(_)
                | Stage::Parked
                | Stage::Finishing
        ) {
            self.dir.clear(self.call.pid);
            if !state.logged_end {
                let mut line = self.call.log_line(Event::Aborted);
                line.job = state.job.clone();
                Journal { path: &self.log }.write(&line);
            }
        }
        for path in &self.temporary {
            let _ = fs::remove_dir_all(path).or_else(|_| fs::remove_file(path));
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

    /// Hands the first of them to the stopper, on a thread of its own. A hangup is the caller's
    /// connection going, which a job its call let go of outlives.
    pub fn listen(self, stopper: Arc<Stopper>) {
        thread::spawn(move || {
            loop {
                let mut signal = 0;
                // SAFETY: the set was built by `block`, and sigwait writes one int.
                unsafe { libc::sigwait(&self.set, &mut signal) };
                if signal != libc::SIGHUP || !stopper.state().handed {
                    stopper.signalled(signal)
                }
            }
        });
    }
}

/// Every process the job started, in its group or outside it, as `setsid` starts one: each names
/// the job in its environment.
pub fn left_by(job: &JobId) -> Vec<u32> {
    let me = std::process::id();
    Host::processes()
        .iter()
        .map(|process| process.pid)
        .filter(|&pid| pid != me)
        .filter(|&pid| Host::variable(pid, "DIBS_JOB").as_deref() == Some(job.as_str()))
        .collect()
}
