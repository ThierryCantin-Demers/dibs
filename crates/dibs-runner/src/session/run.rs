use crate::{
    call::{Journal, Received},
    channel::{Caller, Channel},
    clock::{Deadline, Moment, Span},
    history::{History, Key, Scope},
    job::{
        Cap, Environment, Guard, HoldFifo, Job, LogRead, Output, Ports, Readiness, Services, Start,
    },
    lock::{Hold, Kind, Lock, LockDir},
    machine::Site,
    platform::{Host, Platform as _},
    queue::Queue,
    series::Binding,
    session::{
        ended::{Ended, Tally},
        laid::{Begins, Laid, Layout},
    },
    settings::Settings,
    sink::Sink,
    status::Look,
    stop::{Stage, State, Stopper},
    tree::{Mark, Stepping},
};
use dibs_format::{
    By, Event, Exit, HistoryLine, JobId, Mode,
    wire::{MaxFrom, Record},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, MutexGuard},
    time::{Duration, Instant},
};

/// How long a job is given to stop once its cap has passed.
const JOB_GRACE: Duration = Duration::from_secs(30);
/// A job that waited this long is told when it got the lock.
const SAY_ACQUIRED_AFTER: u64 = 5;
/// The log is cut back to its last lines once it outgrows the bound.
const LOG_BOUND: usize = 20000;
const LOG_KEPT: usize = 10000;
/// History needs this many runs of a job before it may raise the job's cap.
const RUNS_FOR_A_CAP: usize = 3;
/// What a command bash could not start exits with.
pub const NOT_STARTED: i32 = 127;

/// A call being served: the call itself, the machine, its lock directory, what stops the call,
/// where it speaks, and the machine's settings.
#[derive(Clone, Copy)]
pub struct Serving<'a> {
    pub machine: &'a Site,
    pub dir: &'a LockDir,
    pub stopper: &'a Arc<Stopper>,
    pub call: &'a Received,
    pub sink: &'a Sink,
    pub settings: &'a Settings,
}

impl Serving<'_> {
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
    held: Option<HoldFifo>,
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
struct Begun {
    dir: PathBuf,
    log: Option<PathBuf>,
}

impl Begun {
    /// The log made when nothing was written to it, so `dibs out` finds one.
    fn touch_log(&self) {
        if let Some(log) = &self.log {
            let _ = fs::OpenOptions::new().create(true).append(true).open(log);
        }
    }
}

/// What a job was given besides its command, and how the call ended.
pub struct Hosted {
    /// The command's status, or the one dibs gave the call.
    pub status: i32,
    pub ports: Ports,
    pub services: Option<Services>,
    /// A port or a service failed the call, which ends with 77.
    pub failed: bool,
    /// Who gave the status when the job's tree ended the call before its command.
    pub laid: Option<By>,
    /// The cap stopped the call, rather than its command ending 124 itself.
    pub overran: bool,
}

impl Hosted {
    /// The trailer's lines for the ports, then the services.
    pub fn lines(&self, host: &str) -> String {
        let services = self.services.as_ref().map(|s| s.lines(host));
        format!("{}{}", self.ports.lines(host), services.unwrap_or_default())
    }
}

/// A shared job, a benchmark, a transfer or a sweep: queued, run under the lock, and told.
pub struct Run<'a> {
    at: Serving<'a>,
}

impl<'a> Run<'a> {
    pub fn new(at: Serving<'a>) -> Self {
        Run { at }
    }

    /// Queues, takes the lock, runs the job, and says how it went.
    pub fn serve(&self, mut environment: Environment, caller: Caller) -> i32 {
        let request = &self.at.call.request;
        let binding = (self.at.call.mode() == Mode::Bench
            && !request.watch.hold
            && self.at.settings.machine_series)
            .then(|| Binding {
                path: self.at.machine.history.with_file_name("cards"),
                call: self.at.call,
                host: &self.at.machine.host,
            });
        if let Some(Err(refused)) = binding.as_ref().map(Binding::check) {
            self.at.sink.say(&refused.to_string());
            return Exit::Refused.status();
        }
        let arrived = match self.arrive() {
            Ok(arrived) => arrived,
            Err(code) => return code,
        };
        let history = History::load(&self.at.machine.history);
        let max = self.cap(&history);
        match caller {
            Caller::Channel(channel) if !request.watch.off => channel.watch(
                request.watch.lease,
                Arc::clone(self.at.stopper),
                arrived.held.clone(),
            ),
            Caller::Channel(_) => {}
            Caller::Stdout => Channel::hangup(Arc::clone(self.at.stopper)),
        }
        let acquired = match self.acquire(&history, &arrived) {
            Ok(acquired) => acquired,
            Err(code) => return code,
        };
        let mut state = self.at.stopper.state();
        let begun = match self.begin(&arrived, &acquired, &mut environment, &mut state) {
            Ok(begun) => begun,
            Err(code) => return code,
        };
        let hosted = self.host(state, &arrived, &begun, environment, max);
        self.finish(Finish {
            arrived: &arrived,
            acquired,
            begun: &begun,
            hosted: &hosted,
            max,
            binding: binding.as_ref(),
        })
    }

    /// Joins the queue: the waiting record, the batch it is a step of, the fifo a hold waits on.
    fn arrive(&self) -> Result<Arrived, i32> {
        let request = &self.at.call.request;
        let pid = self.at.call.pid;
        let start = Moment::epoch_now();
        let job = job_id(start, pid);
        let held = match request.watch.hold {
            true => match HoldFifo::make(self.at.dir, pid) {
                Ok(held) => Some(held),
                Err(_) => {
                    self.at.sink.say(&format!(
                        "dibs: could not make {}, so nothing is held.\n",
                        self.at.dir.file("hold", pid).display()
                    ));
                    return Err(Exit::NoLock.status());
                }
            },
            false => None,
        };
        let mut state = self.at.stopper.state();
        self.at
            .dir
            .write(Kind::Waiting, &self.at.call.lock_record(start, &job));
        if let Some(batch) = request.batch.as_deref().filter(|b| !b.is_empty()) {
            let _ = fs::write(self.at.dir.file("batch", pid), format!("{batch}\n"));
        }
        let mut line = self.at.call.log_line(Event::Arrived);
        line.job = Some(job.clone());
        self.at.journal().write(&line);
        state.stage = Stage::Queued;
        state.job = Some(job.clone());
        Ok(Arrived { start, job, held })
    }

    /// Waits its turn: through the gate unless it may go around, then for the lock, within
    /// `--wait` when there is one.
    fn acquire(&self, history: &History, arrived: &Arrived) -> Result<Acquired, i32> {
        let request = &self.at.call.request;
        let pid = self.at.call.pid;
        let lock = match Lock::open(self.at.dir) {
            Ok(lock) => lock,
            Err(e) => {
                self.at.sink.say(&format!(
                    "dibs: the lock in {} could not be opened: {e}. Nothing was run.\n",
                    self.at.dir.path.display()
                ));
                self.at.dir.clear(pid);
                return Err(Exit::NoLock.status());
            }
        };
        let queue = Queue {
            dir: self.at.dir,
            history,
            call: self.at.call,
        };
        let hold = match self.at.call.mode() {
            Mode::Bench => Hold::Exclusive,
            _ => Hold::Shared,
        };
        let look = Look {
            machine: self.at.machine,
            dir: self.at.dir,
            history,
            settings: self.at.settings,
            asking: pid,
        };
        let shown = || look.text(&look.status(request.verbose), request.tty);
        let deadline = request
            .wait
            .map(|wait| Instant::now() + Duration::from_secs(wait));
        let passed = match queue.may_bypass(self.at.settings) {
            true => {
                let mut line = self.at.call.log_line(Event::Bypassed);
                line.job = Some(arrived.job.clone());
                self.at.journal().write(&line);
                Ok(true)
            }
            false => lock.pass_gate(deadline),
        };
        let waited = passed.and_then(|passed| match passed {
            true => {
                if !lock.free(hold) {
                    self.at.sink.say(&queue.line());
                    if request.verbose {
                        self.at.sink.say(&shown());
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
                self.at.sink.say(&format!(
                    "dibs: the lock in {} could not be taken: {e}. Nothing was run.\n",
                    self.at.dir.path.display()
                ));
                self.at.dir.clear(pid);
                return Err(Exit::NoLock.status());
            }
        };
        lock.leave_gate();
        if let Some(said) = gave_up {
            self.at.sink.say(&said);
            self.at.sink.say(&shown());
            let mut state = self.at.stopper.state();
            self.abandon(&arrived.job, &mut state);
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
        arrived: &Arrived,
        acquired: &Acquired,
        environment: &mut Environment,
        state: &mut MutexGuard<State>,
    ) -> Result<Begun, i32> {
        let mode = self.at.call.mode();
        let job = &arrived.job;
        self.at
            .dir
            .hold(&self.at.call.lock_record(acquired.at, job));
        if acquired.waited >= SAY_ACQUIRED_AFTER {
            self.at.sink.say(&format!(
                "dibs: acquired the {mode} lock after {}\n",
                Span(acquired.waited)
            ));
        }
        Host::stay_awake(self.at.call.pid);
        if mode == Mode::Bench {
            environment.set("DIBS_STATE", Host::machine_state());
        }
        let transfer = mode == Mode::Rsh;
        let dir = self.at.machine.jobs().join(job.as_str());
        let kept = match transfer {
            true => Ok(()),
            false => fs::create_dir_all(&dir)
                .and_then(|()| Run::write_command(&dir.join("cmd"), &self.at.call.work)),
        };
        if let Err(e) = &kept
            && no_room(e)
        {
            self.at.sink.say(&format!(
                "dibs: {} on {host} is full or over quota, so nothing ran there.\n  \
                 dibs --on {host} --gc --dry-run says what fills it. Tell the person you work for,\n  \
                 and do not delete anything on a shared machine to make room.\n",
                self.at.machine.scratch.display(),
                host = self.at.machine.host
            ));
            self.abandon(job, state);
            return Err(Exit::NoRoom.status());
        }
        let log = (!transfer && kept.is_ok()).then(|| dir.join("log"));
        if log.is_some() {
            environment.set("DIBS_JOB", job.to_string());
        }
        Ok(Begun { dir, log })
    }

    /// The ports and services the job asked for, then the job itself, to its end.
    fn host(
        &self,
        mut state: MutexGuard<'a, State>,
        arrived: &Arrived,
        begun: &Begun,
        mut environment: Environment,
        max: u64,
    ) -> Hosted {
        let request = &self.at.call.request;
        let pid = self.at.call.pid;
        let output = match (&begun.log, request.stream) {
            _ if self.at.call.mode() == Mode::Rsh => Output::Through,
            (Some(log), true) => Output::Stream(log),
            (Some(log), false) => Output::Log(log),
            (None, _) => Output::Caller,
        };
        let deadline = Deadline::after((max > 0).then(|| Duration::from_secs(max)));
        let ports = Ports::take(&request.ports, self.at.settings.ports, self.at.dir, pid);
        for picked in &ports.picked {
            environment.port(picked);
        }
        let mut hosted = Hosted {
            status: NOT_STARTED,
            ports,
            services: None,
            failed: false,
            laid: None,
            overran: false,
        };
        let layout = Layout::new(self.at, output);
        let mut spot = None;
        if let Some(tree) = &request.tree
            && hosted.ports.complete()
        {
            state.stage = Stage::Preparing(None);
            drop(state);
            let laid = layout.lay_out(tree, &mut environment, deadline);
            state = self.at.stopper.state();
            match laid {
                Laid::Run(laid) => spot = laid,
                Laid::Done { status, by } => {
                    drop(state);
                    begun.touch_log();
                    hosted.laid = Some(by);
                    hosted.overran = status == Exit::Overran.status();
                    hosted.status = status;
                    return hosted;
                }
            }
        }
        let say = |text: &str| output.tell(self.at.sink, text);
        let step = request.tree.as_ref().and_then(|t| t.step.as_ref());
        let job_dir = begun.log.as_ref().map(|_| begun.dir.as_path());
        let stepping = spot.as_ref().map(|spot| Stepping::new(spot, job_dir, &say));
        let mut running = None;
        if let (Some(step), Some(stepping)) = (step, &stepping) {
            match layout.step_begins(step, stepping) {
                Begins::Refused => {
                    drop(state);
                    begun.touch_log();
                    hosted.laid = Some(By::Dibs);
                    hosted.status = Exit::TargetRebuilt.status();
                    return hosted;
                }
                Begins::Runs(started) => running = Some(started),
            }
        }
        if !hosted.ports.complete() {
            self.at.sink.say(&format!(
                "dibs: no free port in {} on {}, so the command did not run.\n",
                self.at.settings.ports, self.at.machine.host
            ));
            hosted.failed = true;
        } else if !request.services.is_empty() {
            let mut services = Services::start(Start {
                specs: &request.services,
                environment: &environment,
                job_dir: begun.log.as_ref().map(|_| begun.dir.as_path()),
                record: self.at.dir.file("with", pid),
                sink: self.at.sink,
            });
            state.services = services.pids();
            state.stage = Stage::Starting;
            drop(state);
            let ready = services.ready(
                request.ready_within,
                &Readiness {
                    ports: &hosted.ports,
                    environment: &environment,
                    sink: self.at.sink,
                },
            );
            if !ready {
                services.stop();
            }
            hosted.failed = !ready;
            hosted.services = Some(services);
            state = self.at.stopper.state();
        }
        let status = match hosted.failed {
            true => {
                drop(state);
                begun.touch_log();
                Exit::ServiceFailed.status()
            }
            false => {
                let command = arrived.held.as_ref().map(HoldFifo::command);
                let command = command.as_deref().unwrap_or(&self.at.call.work);
                let mark = running.as_ref().and_then(|r| r.mark.as_ref());
                match Job::spawn(command, &environment, output, self.at.sink, mark) {
                    Ok(work) => {
                        state.stage = Stage::Running(work.pid);
                        drop(state);
                        let cap = deadline.left().map(|after| Cap {
                            after,
                            grace: JOB_GRACE,
                        });
                        self.work(work, cap, arrived.held.is_some(), &mut hosted)
                    }
                    Err(e) => {
                        drop(state);
                        self.at
                            .sink
                            .say(&format!("dibs: bash could not start: {e}\n"));
                        NOT_STARTED
                    }
                }
            }
        };
        if let Some(services) = &mut hosted.services {
            services.stop();
        }
        hosted.status = status;
        if let (Some(step), Some(stepping)) = (step, &stepping)
            && let Some(running) = running
            && layout.step_ends(step, stepping, &mut hosted.status, hosted.overran, running)
        {
            hosted.laid = Some(By::Dibs);
        }
        hosted
    }

    /// Waits for the job, with its services watched: one that ends first stops the job, and the
    /// call ends 77. A service found ended once the job has is taken to have ended first, as a
    /// shell's `wait -n` takes the earlier of two children already gone. A hold's caller learns
    /// here that the lock is held, and on which ports.
    fn work(&self, work: Job, cap: Option<Cap>, holding: bool, hosted: &mut Hosted) -> i32 {
        let guard = hosted.services.as_mut().map(|s| s.guard(work.pid));
        if holding {
            self.at
                .sink
                .record(Record::Holding(hosted.ports.picked.clone()));
        }
        let end = work.wait(cap);
        hosted.overran = end.capped;
        let status = end.status;
        let ended = guard
            .and_then(Guard::over)
            .or_else(|| hosted.services.as_ref().and_then(Services::ended));
        match (ended, hosted.services.as_mut()) {
            (Some(service), Some(services)) => {
                let code = services.status(service);
                services.failed(
                    service,
                    &format!("exited {code} while the command ran"),
                    "the command was stopped",
                    self.at.sink,
                );
                hosted.failed = true;
                Exit::ServiceFailed.status()
            }
            _ => status,
        }
    }

    /// Says how the job ended, keeps what `dibs out` reads, lets the lock go, and only then tells
    /// the caller, who cannot hold the lock by reading slowly.
    fn finish(&self, end: Finish) -> i32 {
        let request = &self.at.call.request;
        let mode = self.at.call.mode();
        let job = &end.arrived.job;
        let cancelled = self
            .at
            .call
            .batch_id()
            .is_some_and(|b| self.at.dir.cancelled(b));
        let status = if cancelled {
            Exit::Cancelled.status()
        } else {
            end.hosted.status
        };
        let ran = Moment::epoch_now().saturating_sub(end.acquired.at);
        {
            let mut state = self.at.stopper.state();
            state.stage = Stage::Finishing;
            let mut line = self.at.call.log_line(Event::Finished);
            line.queued = Some(end.acquired.waited);
            line.ran = Some(ran);
            line.exit = Some(status);
            line.job = Some(job.clone());
            self.at.journal().write(&line);
            state.logged_end = true;
        }
        let ended = end.begun.log.as_ref().map(|log| {
            let by = match Exit::of_code(status) {
                _ if !cancelled && let Some(by) = end.hosted.laid => by,
                Some(Exit::Overran) if end.hosted.overran => By::Dibs,
                Some(Exit::Cancelled) if cancelled => By::Dibs,
                Some(Exit::ServiceFailed) if end.hosted.failed => By::Dibs,
                _ => By::Command,
            };
            Ended::new(
                self.at,
                Tally {
                    job,
                    log,
                    read: LogRead::of(log),
                    job_dir: &end.begun.dir,
                    hosted: end.hosted,
                    waited: end.acquired.waited,
                    ran,
                    status,
                    by,
                },
            )
        });
        if let Some(ended) = &ended {
            ended.keep();
        }
        self.at.journal().trim(LOG_BOUND, LOG_KEPT);
        if status == 0
            && let Some(binding) = end.binding
        {
            binding.record();
        }
        if status == 0 {
            History::append(
                &self.at.machine.history,
                &HistoryLine {
                    mode,
                    label: self.at.call.label().clone(),
                    seconds: Moment::epoch_now().saturating_sub(end.acquired.at),
                    agent: Some(self.at.call.agent.clone()),
                    fingerprint: request.fingerprint.clone(),
                },
            );
        }
        self.at.dir.clear(self.at.call.pid);
        drop(end.acquired.lock);
        if let Some(ended) = &ended {
            ended.report();
        }
        if status == Exit::Overran.status() && end.hosted.overran {
            self.overran(end.max);
        }
        status
    }

    /// `--max`, raised to twice what 90% of this job's own runs took when nobody chose it, so work
    /// that always runs long is not killed at its mode's default. Only a shared job or a
    /// benchmark: a transfer and a sweep keep their mode's cap.
    fn cap(&self, history: &History) -> u64 {
        let request = &self.at.call.request;
        let max = request.max;
        let a_job = matches!(self.at.call.mode(), Mode::Shared | Mode::Bench);
        if !a_job || request.max_from != MaxFrom::Default || request.watch.hold || max == 0 {
            return max;
        }
        let Some(estimate) = history.estimate_kept(Key {
            mode: self.at.call.mode(),
            label: self.at.call.label(),
            agent: Some(&self.at.call.agent),
            fingerprint: request.fingerprint.as_deref(),
        }) else {
            return max;
        };
        if estimate.scope != Scope::This
            || estimate.runs < RUNS_FOR_A_CAP
            || estimate.high * 2 <= max
        {
            return max;
        }
        let raised = estimate.high * 2;
        self.at.sink.say(&format!(
            "dibs: 90% of {} runs of this took up to {}, so it may hold the lock for {} rather than {}. --max sets it.\n",
            estimate.runs,
            Span(estimate.high),
            Span(raised),
            Span(max)
        ));
        raised
    }

    /// Says what to do about an overrun: run it again, since a compile picks up where it stopped.
    fn overran(&self, max: u64) {
        let mut said = format!(
            "dibs: stopped after holding the lock for {max}s, which is --max for a {} job.\n  \
             Nothing is wrong with it; it was simply told to hold no longer than that.\n",
            self.at.call.mode()
        );
        if !self.at.call.request.watch.hold {
            said.push_str(
                "  Run it again; a compile picks up from the crates that already finished, since the\n  \
                 build cache outlives the job. Anything else starts over.\n",
            );
        }
        said.push_str(&format!(
            "  If it truly needs one long run, say so:  --max {}\n",
            max * 2
        ));
        self.at.sink.say(&said);
    }

    /// Leaves the queue or the lock without running anything, and says so in the log.
    fn abandon(&self, job: &JobId, state: &mut MutexGuard<State>) {
        self.at.dir.clear(self.at.call.pid);
        let mut line = self.at.call.log_line(Event::Aborted);
        line.job = Some(job.clone());
        self.at.journal().write(&line);
        state.logged_end = true;
    }

    /// What the job runs, whose time marks the job's start: what it writes is newer.
    fn write_command(path: &Path, command: &str) -> io::Result<()> {
        fs::write(path, format!("{command}\n"))?;
        Mark(path).passed()
    }
}

/// What a job's end is told from.
struct Finish<'a> {
    arrived: &'a Arrived,
    acquired: Acquired,
    begun: &'a Begun,
    hosted: &'a Hosted,
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

/// A job's id: when it arrived, in the machine's zone, and the pid of the runner that took it.
fn job_id(start: u64, pid: u32) -> JobId {
    JobId::new(format!("{}-{pid}", Moment::at(start).compact()))
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
