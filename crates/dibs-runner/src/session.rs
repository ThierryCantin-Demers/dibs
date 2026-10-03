use crate::{
    call::{Call, Journal, one_line},
    channel::Channel,
    clock::{Moment, Span},
    history::{History, Key, Scope},
    job::{
        Cap, Digest, Environment, Guard, Held, Job, Output, Ports, Readiness, Repeat, Services,
        Start, Unpinned, built, job_id,
    },
    kept::Kept,
    lock::{Hold, Kind, Lock, LockDir},
    machine::{Machine, line_count},
    platform::{Host, Platform as _},
    queue::Queue,
    settings::Settings,
    sink::Sink,
    status::Look,
    stop::{Signals, Stage, Stopper},
    views::Views,
};
use dibs_format::{
    By, Event, HistoryLine, JobId, JobMeta, Mode,
    wire::{MaxFrom, Record, Request, Trailer},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime},
};

/// How long a peek's command is given to stop once its cap has passed, and a job's.
const PEEK_GRACE: Duration = Duration::from_secs(5);
const JOB_GRACE: Duration = Duration::from_secs(30);
/// A job that waited this long is told when it got the lock.
const SAY_ACQUIRED_AFTER: u64 = 5;
/// History needs this many runs of a job before it may raise the job's cap.
const RUNS_FOR_A_CAP: usize = 3;
/// The log is cut back to its last lines once it outgrows the bound.
const LOG_BOUND: usize = 20000;
const LOG_KEPT: usize = 10000;
/// What a command bash could not start exits with.
const NOT_STARTED: i32 = 127;
/// A port or a service failed the call.
const SERVICE_FAILED: i32 = 77;
/// Longer than the coarsest tick a file's time is stamped by.
const FILE_TICK: Duration = Duration::from_millis(11);

/// One request in, frames out, the exit. A transfer's request is followed by rsync's own stream
/// both ways, so after saying it has read it the runner frames nothing, and exits with the call.
pub fn serve() -> i32 {
    let signals = Signals::block();
    let sink = Sink::frames();
    let mut channel = match Channel::stdin() {
        Ok(channel) => channel,
        Err(e) => {
            sink.say(&format!("dibs-runner: stdin could not be read: {e}\n"));
            sink.exit(2);
            return 2;
        }
    };
    let code = match channel.request() {
        Ok(request) if request.mode == Mode::Rsh => {
            sink.record(Record::Transferring);
            return Session::new(request, Sink::plain()).serve(None, signals);
        }
        Ok(request) => Session::new(request, sink.clone()).serve(Some(channel), signals),
        Err(e) => {
            sink.say(&format!("dibs-runner: {e}\n"));
            2
        }
    };
    sink.exit(code);
    code
}

/// One call on this machine.
pub struct Session {
    call: Call,
    sink: Sink,
    settings: Settings,
}

impl Session {
    pub fn new(request: Request, sink: Sink) -> Session {
        Session {
            call: Call::of(request),
            sink,
            settings: Settings::from_env(),
        }
    }

    /// Runs the call to its end; the caller's channel, where it has one, is watched while it is
    /// queued and runs.
    pub fn serve(&self, channel: Option<Channel>, signals: Signals) -> i32 {
        let machine = match Machine::set_up() {
            Ok(machine) => machine,
            Err(unwritable) => {
                self.sink.say(&unwritable.said());
                return 71;
            }
        };
        let dir = LockDir {
            path: machine.lock_dir.clone(),
        };
        let stopper = Arc::new(Stopper::new(
            self.call.clone(),
            dir.clone(),
            machine.log.clone(),
            self.sink.clone(),
        ));
        signals.listen(Arc::clone(&stopper));
        let history = History::load(&machine.history);
        let views = Views {
            look: Look {
                machine: &machine,
                dir: &dir,
                history: &history,
                settings: &self.settings,
                asking: self.call.pid,
            },
            sink: &self.sink,
            request: &self.call.request,
        };
        match self.call.mode() {
            Mode::Status => return views.status(),
            Mode::Watch => return views.watch(channel),
            Mode::Log => return views.log(),
            _ => {}
        }
        let environment = match Environment::of(&machine, self.call.request.card.as_ref()) {
            Ok(environment) => environment,
            Err(Unpinned(said)) => {
                self.sink.say(&said);
                return 2;
            }
        };
        if let Some(batch) = self.call.batch_id()
            && dir.cancelled(batch)
        {
            self.sink.say(&format!(
                "dibs: batch {batch} was cancelled with dibs --kill, so this step does not run.\n"
            ));
            let mut line = self.call.log_line(Event::Refused);
            line.command = format!(
                "refused, batch cancelled: {}",
                one_line(&self.call.request.command, 160)
            );
            Journal { path: &machine.log }.write(&line);
            return 76;
        }
        let at = Place {
            machine: &machine,
            dir: &dir,
            stopper: &stopper,
        };
        let kept = Kept {
            machine: &machine,
            dir: &dir,
            sink: &self.sink,
            tty: self.call.request.tty,
        };
        match self.call.mode() {
            Mode::Peek => self.peek(&at, &environment),
            Mode::Shared | Mode::Bench | Mode::Rsh => self.run(&at, environment, channel),
            Mode::Out => kept.out(self.call.label().as_str()),
            Mode::Fetch => kept.fetch(self.call.label().as_str()),
            mode => {
                self.sink
                    .say(&format!("dibs-runner: no {mode} call is served here\n"));
                2
            }
        }
    }

    /// A peek runs beside whatever is measured, with no lock, and every one is logged: the log has
    /// to say what ran beside which run.
    fn peek(&self, at: &Place, environment: &Environment) -> i32 {
        let request = &self.call.request;
        let start = Moment::epoch_now();
        let cap = (request.max > 0).then(|| Cap {
            after: Duration::from_secs(request.max),
            grace: PEEK_GRACE,
        });
        let mut state = at.stopper.state();
        let job = match Job::spawn(&request.command, environment, Output::Caller, &self.sink) {
            Ok(job) => job,
            Err(e) => {
                self.sink.say(&format!("dibs: bash could not start: {e}\n"));
                return NOT_STARTED;
            }
        };
        state.stage = Stage::Peeking(job.pid);
        drop(state);
        let status = job.wait(cap);
        at.stopper.state().stage = Stage::Setup;
        let took = Moment::epoch_now().saturating_sub(start);
        let journal = Journal {
            path: &at.machine.log,
        };
        let peeked = |event| {
            let mut line = self.call.log_line(event);
            line.ran = Some(took);
            line.exit = Some(status);
            let command = one_line(&request.command, 200);
            if !command.is_empty() {
                line.command = command;
            }
            line
        };
        journal.write(&peeked(Event::Peek));
        if took >= self.settings.peek_warn {
            self.sink.say(&format!(
                "dibs: that --peek took {} and ran with no lock, beside\n  \
                 whatever is being measured. Anything that costs time belongs in\n  \
                 'dibs <command>', which takes the shared lock.\n",
                Span(took)
            ));
            journal.write(&peeked(Event::PeekSlow));
        }
        status
    }

    /// A shared job or a benchmark: queue, take the lock, run the job, and say how it went.
    fn run(&self, at: &Place, mut environment: Environment, channel: Option<Channel>) -> i32 {
        let request = &self.call.request;
        let mode = self.call.mode();
        let pid = self.call.pid;
        let journal = Journal {
            path: &at.machine.log,
        };
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
                    return 71;
                }
            },
            false => None,
        };
        {
            let mut state = at.stopper.state();
            at.dir
                .write(Kind::Waiting, &self.call.lock_record(start, &job));
            if let Some(batch) = request.batch.as_deref().filter(|b| !b.is_empty()) {
                let _ = fs::write(at.dir.file("batch", pid), format!("{batch}\n"));
            }
            let mut line = self.call.log_line(Event::Arrived);
            line.job = Some(job.clone());
            journal.write(&line);
            state.stage = Stage::Queued;
            state.job = Some(job.clone());
        }
        let history = History::load(&at.machine.history);
        let max = self.cap(&history);
        if let Some(channel) = channel
            && !request.watch.off
        {
            channel.watch(request.watch.lease, Arc::clone(at.stopper), held.clone());
        }
        let transfer = mode == Mode::Rsh;
        if transfer {
            Channel::hangup(Arc::clone(at.stopper));
        }

        let lock = match Lock::open(at.dir) {
            Ok(lock) => lock,
            Err(e) => {
                self.sink.say(&format!(
                    "dibs: the lock in {} could not be opened: {e}. Nothing was run.\n",
                    at.dir.path.display()
                ));
                at.dir.clear(pid);
                return 71;
            }
        };
        let queue = Queue {
            dir: at.dir,
            history: &history,
            call: &self.call,
        };
        let hold = match mode {
            Mode::Bench => Hold::Exclusive,
            _ => Hold::Shared,
        };
        let look = Look {
            machine: at.machine,
            dir: at.dir,
            history: &history,
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
                line.job = Some(job.clone());
                journal.write(&line);
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
                return 71;
            }
        };
        lock.leave_gate();
        if let Some(said) = gave_up {
            self.sink.say(&said);
            self.sink.say(&shown());
            let mut state = at.stopper.state();
            at.dir.clear(pid);
            let mut line = self.call.log_line(Event::Aborted);
            line.job = Some(job.clone());
            journal.write(&line);
            state.logged_end = true;
            return 75;
        }

        let acquired = Moment::epoch_now();
        let waited = acquired.saturating_sub(start);
        let mut state = at.stopper.state();
        at.dir.hold(&self.call.lock_record(acquired, &job));
        if waited >= SAY_ACQUIRED_AFTER {
            self.sink.say(&format!(
                "dibs: acquired the {mode} lock after {}\n",
                Span(waited)
            ));
        }
        Host::stay_awake(pid);
        if mode == Mode::Bench {
            environment.set("DIBS_STATE", Host::machine_state());
        }
        let job_dir = at.machine.jobs().join(job.as_str());
        let log = (!transfer
            && fs::create_dir_all(&job_dir)
                .and_then(|()| Session::write_command(&job_dir.join("cmd"), &request.command))
                .is_ok())
        .then(|| job_dir.join("log"));
        if log.is_some() {
            environment.set("DIBS_JOB", job.to_string());
        }
        let output = match (&log, request.stream) {
            _ if transfer => Output::Through,
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
        };
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
                job_dir: log.as_ref().map(|_| job_dir.as_path()),
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
                if let Some(log) = &log {
                    let _ = fs::OpenOptions::new().create(true).append(true).open(log);
                }
                SERVICE_FAILED
            }
            false => {
                let command = held.as_ref().map(Held::command);
                let command = command.as_deref().unwrap_or(&request.command);
                match Job::spawn(command, &environment, output, &self.sink) {
                    Ok(work) => {
                        state.stage = Stage::Running(work.pid);
                        drop(state);
                        self.work(work, cap, held.is_some(), &mut hosted)
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
        let cancelled = self.call.batch_id().is_some_and(|b| at.dir.cancelled(b));
        let status = if cancelled { 76 } else { status };
        let ran = Moment::epoch_now().saturating_sub(acquired);
        {
            let mut state = at.stopper.state();
            state.stage = Stage::Finishing;
            let mut line = self.call.log_line(Event::Finished);
            line.queued = Some(waited);
            line.ran = Some(ran);
            line.exit = Some(status);
            line.job = Some(job.clone());
            journal.write(&line);
            state.logged_end = true;
        }
        if let Some(log) = &log {
            let by = match status {
                124 if max > 0 => By::Dibs,
                76 if cancelled => By::Dibs,
                SERVICE_FAILED if hosted.failed => By::Dibs,
                78 if fs::read_to_string(log)
                    .is_ok_and(|l| l.lines().any(|l| l == "DIBS-REFUSED")) =>
                {
                    By::Dibs
                }
                _ => By::Command,
            };
            Ended {
                session: self,
                at,
                job: &job,
                log,
                job_dir: &job_dir,
                hosted: &hosted,
                waited,
                ran,
                status,
                by,
            }
            .report();
        }
        crate::history::Trim {
            path: &at.machine.log,
            bound: LOG_BOUND,
            kept: LOG_KEPT,
        }
        .run();
        if status == 124 {
            self.overran(max);
        }
        if status == 0 {
            History::append(
                &at.machine.history,
                &HistoryLine {
                    mode,
                    label: self.call.label().clone(),
                    seconds: Moment::epoch_now().saturating_sub(acquired),
                    agent: Some(self.call.agent.clone()),
                    fingerprint: request.fingerprint.clone(),
                },
            );
        }
        at.dir.clear(pid);
        drop(lock);
        status
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
                SERVICE_FAILED
            }
            _ => status,
        }
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

    /// `--max`, raised to twice what 90% of this job's own runs took when nobody chose it, so work
    /// that always runs long is not killed at its mode's default.
    fn cap(&self, history: &History) -> u64 {
        let request = &self.call.request;
        let max = request.max;
        if request.max_from != MaxFrom::Default || request.watch.hold || max == 0 {
            return max;
        }
        let Some(estimate) = history.estimate(Key {
            mode: self.call.mode(),
            label: self.call.label(),
            agent: Some(&self.call.agent),
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
        self.sink.say(&format!(
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
            self.call.mode()
        );
        if !self.call.request.watch.hold {
            said.push_str(
                "  Run it again; a compile picks up from the crates that already finished, since the\n  \
                 build cache outlives the job. Anything else starts over.\n",
            );
        }
        said.push_str(&format!(
            "  If it truly needs one long run, say so:  --max {}\n",
            max * 2
        ));
        self.sink.say(&said);
    }
}

/// Where a call runs: the machine, its lock directory, and what stops the call.
struct Place<'a> {
    machine: &'a Machine,
    dir: &'a LockDir,
    stopper: &'a Arc<Stopper>,
}

/// How waiting for the lock ended.
enum Waited {
    Held,
    /// `--wait` passed while a benchmark queued ahead held the gate.
    GaveUpAtGate,
    GaveUpAtLock,
}

/// What a job was given besides its command.
struct Hosted {
    ports: Ports,
    services: Option<Services>,
    /// A port or a service failed the call, which ends with 77.
    failed: bool,
}

impl Hosted {
    /// The trailer's lines for the ports, then the services.
    fn lines(&self, host: &str) -> String {
        let services = self.services.as_ref().map(|s| s.lines(host));
        format!("{}{}", self.ports.lines(host), services.unwrap_or_default())
    }
}

/// A job that has ended, which its caller is told about.
struct Ended<'a> {
    session: &'a Session,
    at: &'a Place<'a>,
    job: &'a JobId,
    log: &'a PathBuf,
    job_dir: &'a PathBuf,
    hosted: &'a Hosted,
    waited: u64,
    ran: u64,
    status: i32,
    by: By,
}

impl Ended<'_> {
    /// The digest, then the trailer and what follows it: the same shape every time, on stderr,
    /// where a pipe on the caller's side cannot cut it off.
    fn report(&self) {
        let session = self.session;
        let call = &session.call;
        let host = &self.at.machine.host;
        let lines = line_count(self.log).unwrap_or_default();
        let text = fs::read(self.log).unwrap_or_default();
        if !call.request.stream {
            session.sink.out(
                &Digest {
                    log: &text,
                    head: session.settings.digest_head,
                    tail: session.settings.digest_tail,
                    host,
                    job: self.job,
                    path: self.log,
                }
                .text(),
            );
        }
        let built = built(&text);
        session.sink.record(Record::Trailer(Trailer {
            job: self.job.clone(),
            mode: call.mode(),
            label: call.label().clone(),
            queued: self.waited,
            ran: self.ran,
            exit: self.status,
            by: self.by,
            built,
        }));
        let mut after = String::new();
        if !call.request.watch.hold {
            after.push_str(&format!(
                "  log {host}:{}  ({lines} lines)  dibs --on {host} --out {}\n",
                self.log.display(),
                self.job
            ));
        }
        after.push_str(&self.hosted.lines(host));
        if built == Some(dibs_format::wire::Built::Nothing) {
            after.push_str(
                "  built nothing: cargo compiled 0 crates, so a measurement after this measures the previous binary.\n",
            );
        }
        if self.status != 0
            && !call.request.watch.hold
            && let Some(earlier) = (Repeat {
                jobs: &self.at.machine.jobs(),
                this: self.job_dir,
                window: session.settings.repeat_window,
            })
            .earlier()
        {
            after.push_str(&format!(
                "  this exact command already failed here: job {earlier}. Unchanged, it failed the same way.\n"
            ));
        }
        session.sink.say(&after);
        let meta = JobMeta {
            mode: call.mode(),
            label: call.label().clone(),
            queued: self.waited,
            ran: self.ran,
            exit: self.status,
            by: self.by,
            agent: call.agent.clone(),
            lines: lines as u64,
        };
        let _ = fs::write(self.job_dir.join("meta"), meta.to_string());
    }
}
