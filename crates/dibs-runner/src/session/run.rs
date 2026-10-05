use crate::{
    call::Journal,
    channel::{Caller, Channel},
    clock::{Moment, Span},
    history::History,
    job::{Cap, Environment, Guard, Held, Job, Output, Ports, Readiness, Services, Start, job_id},
    lock::{Hold, Kind, Lock, LockDir},
    machine::{Machine, line_count},
    platform::{Host, Platform as _},
    queue::Queue,
    series::Binding,
    session::{base::Session, ended::Ended, laid::Laid},
    status::Look,
    stop::{Stage, State, Stopper},
};
use dibs_format::{By, Event, Exit, HistoryLine, JobId, Mode, wire::Record};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, MutexGuard},
    thread,
    time::{Duration, Instant, SystemTime},
};

/// How long a job is given to stop once its cap has passed.
const JOB_GRACE: Duration = Duration::from_secs(30);
/// A job that waited this long is told when it got the lock.
const SAY_ACQUIRED_AFTER: u64 = 5;
/// The log is cut back to its last lines once it outgrows the bound.
const LOG_BOUND: usize = 20000;
const LOG_KEPT: usize = 10000;
/// What a command bash could not start exits with.
pub(super) const NOT_STARTED: i32 = 127;
/// Longer than the coarsest tick a file's time is stamped by.
const FILE_TICK: Duration = Duration::from_millis(11);

/// Where a call runs: the machine, its lock directory, and what stops the call.
pub(super) struct Place<'a> {
    pub(super) machine: &'a Machine,
    pub(super) dir: &'a LockDir,
    pub(super) stopper: &'a Arc<Stopper>,
}

impl Place<'_> {
    fn journal(&self) -> Journal<'_> {
        Journal {
            path: &self.machine.log,
        }
    }
}

/// A call in the queue: its job's id, and the fifo a hold waits on.
struct Arrived {
    start: u64,
    job: JobId,
    held: Option<Held>,
}

/// How waiting for the lock ended.
enum Waited {
    Held,
    /// `--wait` passed while a benchmark queued ahead held the gate.
    GaveUpAtGate,
    GaveUpAtLock,
}

/// The lock, taken.
struct Acquired {
    lock: Lock,
    at: u64,
    waited: u64,
}

/// Where a job that holds the lock keeps what it leaves: its directory, and its log unless it is
/// a transfer's.
pub(super) struct Begun {
    pub(super) dir: PathBuf,
    pub(super) log: Option<PathBuf>,
}

/// What a job was given besides its command.
pub(super) struct Hosted {
    pub(super) ports: Ports,
    pub(super) services: Option<Services>,
    /// A port or a service failed the call, which ends with 77.
    pub(super) failed: bool,
    /// Who gave the status when the job's tree ended the call before its command.
    pub(super) laid: Option<By>,
}

impl Hosted {
    /// The trailer's lines for the ports, then the services.
    pub(super) fn lines(&self, host: &str) -> String {
        let services = self.services.as_ref().map(|s| s.lines(host));
        format!("{}{}", self.ports.lines(host), services.unwrap_or_default())
    }
}

impl Session {
    /// A shared job or a benchmark: queue, take the lock, run the job, and say how it went.
    pub(super) fn run(&self, at: &Place, mut environment: Environment, caller: Caller) -> i32 {
        let request = &self.call.request;
        let binding = (self.call.mode() == Mode::Bench
            && !request.watch.hold
            && self.settings.machine_series)
            .then(|| Binding {
                path: at.machine.history.with_file_name("cards"),
                call: &self.call,
                host: &at.machine.host,
            });
        if let Some(Err(refused)) = binding.as_ref().map(Binding::check) {
            self.sink.say(&refused);
            return Exit::Refused.status();
        }
        let arrived = match self.arrive(at) {
            Ok(arrived) => arrived,
            Err(code) => return code,
        };
        let history = History::load(&at.machine.history);
        let max = self.cap(&history);
        match caller {
            Caller::Channel(channel) if !request.watch.off => channel.watch(
                request.watch.lease,
                Arc::clone(at.stopper),
                arrived.held.clone(),
            ),
            Caller::Channel(_) => {}
            Caller::Stdout => Channel::hangup(Arc::clone(at.stopper)),
        }
        let acquired = match self.acquire(at, &history, &arrived) {
            Ok(acquired) => acquired,
            Err(code) => return code,
        };
        let mut state = at.stopper.state();
        let begun = match self.begin(at, &arrived, &acquired, &mut environment, &mut state) {
            Ok(begun) => begun,
            Err(code) => return code,
        };
        let (status, hosted) = self.host(at, state, &arrived, &begun, environment, max);
        self.finish(
            at,
            Finish {
                arrived: &arrived,
                acquired,
                begun: &begun,
                hosted: &hosted,
                status,
                max,
                binding: binding.as_ref(),
            },
        )
    }

    /// Joins the queue: the waiting record, the batch it is a step of, the fifo a hold waits on.
    fn arrive(&self, at: &Place) -> Result<Arrived, i32> {
        let request = &self.call.request;
        let pid = self.call.pid;
        let start = Moment::epoch_now();
        let job = job_id(start, pid);
        let held = match request.watch.hold {
            true => match Held::make(at.dir, pid) {
                Ok(held) => Some(held),
                Err(_) => {
                    self.sink.say(&format!(
                        "dibs: could not make {}, so nothing is held.\n",
                        at.dir.file("hold", pid).display()
                    ));
                    return Err(Exit::NoLock.status());
                }
            },
            false => None,
        };
        let mut state = at.stopper.state();
        at.dir
            .write(Kind::Waiting, &self.call.lock_record(start, &job));
        if let Some(batch) = request.batch.as_deref().filter(|b| !b.is_empty()) {
            let _ = fs::write(at.dir.file("batch", pid), format!("{batch}\n"));
        }
        let mut line = self.call.log_line(Event::Arrived);
        line.job = Some(job.clone());
        at.journal().write(&line);
        state.stage = Stage::Queued;
        state.job = Some(job.clone());
        Ok(Arrived { start, job, held })
    }

    /// Waits its turn: through the gate unless it may go around, then for the lock, within
    /// `--wait` when there is one.
    fn acquire(&self, at: &Place, history: &History, arrived: &Arrived) -> Result<Acquired, i32> {
        let request = &self.call.request;
        let pid = self.call.pid;
        let lock = match Lock::open(at.dir) {
            Ok(lock) => lock,
            Err(e) => {
                self.sink.say(&format!(
                    "dibs: the lock in {} could not be opened: {e}. Nothing was run.\n",
                    at.dir.path.display()
                ));
                at.dir.clear(pid);
                return Err(Exit::NoLock.status());
            }
        };
        let queue = Queue {
            dir: at.dir,
            history,
            call: &self.call,
        };
        let hold = match self.call.mode() {
            Mode::Bench => Hold::Exclusive,
            _ => Hold::Shared,
        };
        let look = Look {
            machine: at.machine,
            dir: at.dir,
            history,
            settings: &self.settings,
            asking: pid,
        };
        let shown = || look.text(&look.status(request.verbose), request.tty);
        let deadline = request
            .wait
            .map(|wait| Instant::now() + Duration::from_secs(wait));
        let passed = match queue.may_bypass(&self.settings) {
            true => {
                let mut line = self.call.log_line(Event::Bypassed);
                line.job = Some(arrived.job.clone());
                at.journal().write(&line);
                Ok(true)
            }
            false => lock.pass_gate(deadline),
        };
        let waited = passed.and_then(|passed| match passed {
            true => {
                if !lock.free(hold) {
                    self.sink.say(&queue.line());
                    if request.verbose {
                        self.sink.say(&shown());
                    }
                }
                lock.take(hold, deadline).map(|taken| match taken {
                    true => Waited::Held,
                    false => Waited::GaveUpAtLock,
                })
            }
            false => Ok(Waited::GaveUpAtGate),
        });
        let wait = request.wait.unwrap_or_default();
        let gave_up = match waited {
            Ok(Waited::Held) => None,
            Ok(Waited::GaveUpAtLock) => Some(format!("dibs: still busy after {wait}s, gave up.\n")),
            Ok(Waited::GaveUpAtGate) => Some(format!(
                "dibs: busy, a benchmark is queued ahead of you. Gave up after {wait}s.\n"
            )),
            Err(e) => {
                lock.leave_gate();
                self.sink.say(&format!(
                    "dibs: the lock in {} could not be taken: {e}. Nothing was run.\n",
                    at.dir.path.display()
                ));
                at.dir.clear(pid);
                return Err(Exit::NoLock.status());
            }
        };
        lock.leave_gate();
        if let Some(said) = gave_up {
            self.sink.say(&said);
            self.sink.say(&shown());
            let mut state = at.stopper.state();
            self.abandon(at, &arrived.job, &mut state);
            return Err(Exit::Busy.status());
        }
        let now = Moment::epoch_now();
        Ok(Acquired {
            lock,
            at: now,
            waited: now.saturating_sub(arrived.start),
        })
    }

    /// Holds the lock: the holder record, the machine kept awake, and the job's directory with
    /// the command it runs, unless the disk has no room for it.
    fn begin(
        &self,
        at: &Place,
        arrived: &Arrived,
        acquired: &Acquired,
        environment: &mut Environment,
        state: &mut MutexGuard<State>,
    ) -> Result<Begun, i32> {
        let mode = self.call.mode();
        let job = &arrived.job;
        at.dir.hold(&self.call.lock_record(acquired.at, job));
        if acquired.waited >= SAY_ACQUIRED_AFTER {
            self.sink.say(&format!(
                "dibs: acquired the {mode} lock after {}\n",
                Span(acquired.waited)
            ));
        }
        Host::stay_awake(self.call.pid);
        if mode == Mode::Bench {
            environment.set("DIBS_STATE", Host::machine_state());
        }
        let transfer = mode == Mode::Rsh;
        let dir = at.machine.jobs().join(job.as_str());
        let kept = match transfer {
            true => Ok(()),
            false => fs::create_dir_all(&dir)
                .and_then(|()| Session::write_command(&dir.join("cmd"), &self.call.work)),
        };
        if let Err(e) = &kept
            && no_room(e)
        {
            self.sink.say(&format!(
                "dibs: {} on {host} is full or over quota, so nothing ran there.\n  \
                 dibs --on {host} --gc --dry-run says what fills it. Tell the person you work for,\n  \
                 and do not delete anything on a shared machine to make room.\n",
                at.machine.scratch.display(),
                host = at.machine.host
            ));
            self.abandon(at, job, state);
            return Err(Exit::NoRoom.status());
        }
        let log = (!transfer && kept.is_ok()).then(|| dir.join("log"));
        if log.is_some() {
            environment.set("DIBS_JOB", job.to_string());
        }
        Ok(Begun { dir, log })
    }

    /// The ports and services the job asked for, then the job itself, to its end.
    fn host<'a>(
        &self,
        at: &Place<'a>,
        mut state: MutexGuard<'a, State>,
        arrived: &Arrived,
        begun: &Begun,
        mut environment: Environment,
        max: u64,
    ) -> (i32, Hosted) {
        let request = &self.call.request;
        let pid = self.call.pid;
        let output = match (&begun.log, request.stream) {
            _ if self.call.mode() == Mode::Rsh => Output::Through,
            (Some(log), true) => Output::Stream(log),
            (Some(log), false) => Output::Log(log),
            (None, _) => Output::Caller,
        };
        let cap = (max > 0).then(|| Cap {
            after: Duration::from_secs(max),
            grace: JOB_GRACE,
        });
        let ports = Ports::take(&request.ports, self.settings.ports, at.dir, pid);
        for picked in &ports.picked {
            environment.port(picked);
        }
        let mut hosted = Hosted {
            ports,
            services: None,
            failed: false,
            laid: None,
        };
        if let Some(tree) = &request.tree
            && hosted.ports.complete()
        {
            state.stage = Stage::Preparing(None);
            drop(state);
            let laid = self.lay_out(at, tree, &mut environment, output);
            state = at.stopper.state();
            if let Laid::Done { status, by } = laid {
                drop(state);
                if let Some(log) = &begun.log {
                    let _ = fs::OpenOptions::new().create(true).append(true).open(log);
                }
                hosted.laid = Some(by);
                return (status, hosted);
            }
        }
        if !hosted.ports.complete() {
            self.sink.say(&format!(
                "dibs: no free port in {} on {}, so the command did not run.\n",
                self.settings.ports, at.machine.host
            ));
            hosted.failed = true;
        } else if !request.services.is_empty() {
            let mut services = Services::start(Start {
                specs: &request.services,
                environment: &environment,
                job_dir: begun.log.as_ref().map(|_| begun.dir.as_path()),
                record: at.dir.file("with", pid),
                sink: &self.sink,
            });
            state.services = services.pids();
            state.stage = Stage::Starting;
            drop(state);
            let ready = services.ready(
                request.ready_within,
                &Readiness {
                    ports: &hosted.ports,
                    environment: &environment,
                    sink: &self.sink,
                },
            );
            if !ready {
                services.stop();
            }
            hosted.failed = !ready;
            hosted.services = Some(services);
            state = at.stopper.state();
        }
        let status = match hosted.failed {
            true => {
                drop(state);
                if let Some(log) = &begun.log {
                    let _ = fs::OpenOptions::new().create(true).append(true).open(log);
                }
                Exit::ServiceFailed.status()
            }
            false => {
                let command = arrived.held.as_ref().map(Held::command);
                let command = command.as_deref().unwrap_or(&self.call.work);
                match Job::spawn(command, &environment, output, &self.sink) {
                    Ok(work) => {
                        state.stage = Stage::Running(work.pid);
                        drop(state);
                        self.work(work, cap, arrived.held.is_some(), &mut hosted)
                    }
                    Err(e) => {
                        drop(state);
                        self.sink.say(&format!("dibs: bash could not start: {e}\n"));
                        NOT_STARTED
                    }
                }
            }
        };
        if let Some(services) = &mut hosted.services {
            services.stop();
        }
        (status, hosted)
    }

    /// Waits for the job, with its services watched: one that ends first stops the job, and the
    /// call ends 77. A service found ended once the job has is taken to have ended first, as a
    /// shell's `wait -n` takes the earlier of two children already gone. A hold's caller learns
    /// here that the lock is held, and on which ports.
    fn work(&self, work: Job, cap: Option<Cap>, holding: bool, hosted: &mut Hosted) -> i32 {
        let guard = hosted.services.as_mut().map(|s| s.guard(work.pid));
        if holding {
            self.sink
                .record(Record::Holding(hosted.ports.picked.clone()));
        }
        let status = work.wait(cap);
        let ended = guard
            .and_then(Guard::over)
            .or_else(|| hosted.services.as_ref().and_then(Services::ended));
        match (ended, hosted.services.as_mut()) {
            (Some(at), Some(services)) => {
                let code = services.status(at);
                services.failed(
                    at,
                    &format!("exited {code} while the command ran"),
                    "the command was stopped",
                    &self.sink,
                );
                hosted.failed = true;
                Exit::ServiceFailed.status()
            }
            _ => status,
        }
    }

    /// Says how the job ended, keeps what `dibs out` reads, lets the lock go, and only then tells
    /// the caller, who cannot hold the lock by reading slowly.
    fn finish(&self, at: &Place, end: Finish) -> i32 {
        let request = &self.call.request;
        let mode = self.call.mode();
        let job = &end.arrived.job;
        let cancelled = self.call.batch_id().is_some_and(|b| at.dir.cancelled(b));
        let status = if cancelled {
            Exit::Cancelled.status()
        } else {
            end.status
        };
        let ran = Moment::epoch_now().saturating_sub(end.acquired.at);
        {
            let mut state = at.stopper.state();
            state.stage = Stage::Finishing;
            let mut line = self.call.log_line(Event::Finished);
            line.queued = Some(end.acquired.waited);
            line.ran = Some(ran);
            line.exit = Some(status);
            line.job = Some(job.clone());
            at.journal().write(&line);
            state.logged_end = true;
        }
        let ended = end.begun.log.as_ref().map(|log| {
            let by = match Exit::of_code(status) {
                _ if !cancelled && let Some(by) = end.hosted.laid => by,
                Some(Exit::Overran) if end.max > 0 => By::Dibs,
                Some(Exit::Cancelled) if cancelled => By::Dibs,
                Some(Exit::ServiceFailed) if end.hosted.failed => By::Dibs,
                Some(Exit::TargetRebuilt)
                    if fs::read_to_string(log)
                        .is_ok_and(|l| l.lines().any(|l| l == "DIBS-REFUSED")) =>
                {
                    By::Dibs
                }
                _ => By::Command,
            };
            Ended {
                session: self,
                machine: at.machine,
                job,
                log,
                lines: line_count(log).unwrap_or_default(),
                job_dir: &end.begun.dir,
                hosted: end.hosted,
                waited: end.acquired.waited,
                ran,
                status,
                by,
            }
        });
        if let Some(ended) = &ended {
            ended.keep();
        }
        at.journal().trim(LOG_BOUND, LOG_KEPT);
        if status == 0
            && let Some(binding) = end.binding
        {
            binding.record();
        }
        if status == 0 {
            History::append(
                &at.machine.history,
                &HistoryLine {
                    mode,
                    label: self.call.label().clone(),
                    seconds: Moment::epoch_now().saturating_sub(end.acquired.at),
                    agent: Some(self.call.agent.clone()),
                    fingerprint: request.fingerprint.clone(),
                },
            );
        }
        at.dir.clear(self.call.pid);
        drop(end.acquired.lock);
        if let Some(ended) = &ended {
            ended.report();
        }
        if status == Exit::Overran.status() {
            self.overran(end.max);
        }
        status
    }

    /// Leaves the queue or the lock without running anything, and says so in the log.
    fn abandon(&self, at: &Place, job: &JobId, state: &mut MutexGuard<State>) {
        at.dir.clear(self.call.pid);
        let mut line = self.call.log_line(Event::Aborted);
        line.job = Some(job.clone());
        at.journal().write(&line);
        state.logged_end = true;
    }

    /// What the job runs, whose time marks the job's start: what it writes is newer. File times
    /// move a tick, up to 10 ms, at a time, so the job starts once a tick has passed the mark.
    fn write_command(path: &Path, command: &str) -> io::Result<()> {
        fs::write(path, format!("{command}\n"))?;
        let marked = fs::metadata(path)?.modified()?;
        if let Ok(left) = (marked + FILE_TICK).duration_since(SystemTime::now()) {
            thread::sleep(left);
        }
        Ok(())
    }
}

/// What a job's end is told from.
struct Finish<'a> {
    arrived: &'a Arrived,
    acquired: Acquired,
    begun: &'a Begun,
    hosted: &'a Hosted,
    status: i32,
    max: u64,
    binding: Option<&'a Binding<'a>>,
}

/// A write that failed for want of space, which no retry here fixes.
fn no_room(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_disk_or_quota_is_no_room_and_a_refusal_is_not() {
        assert!(no_room(&io::Error::from_raw_os_error(libc::ENOSPC)));
        assert!(no_room(&io::Error::from_raw_os_error(libc::EDQUOT)));
        assert!(!no_room(&io::Error::from_raw_os_error(libc::EACCES)));
    }
}
