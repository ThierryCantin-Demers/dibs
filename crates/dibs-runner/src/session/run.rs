use crate::{
    ahead::{Frozen, Preemption, Watch},
    attempt::{Attempts, Receipt},
    bundle::Bundles,
    call::{Journal, ONE_LINE, Received},
    channel::{Caller, Channel},
    clock::{Deadline, Moment, Span},
    closed::{self, Shutter},
    history::{History, Key, Scope},
    image::{Image, Images, Sandbox, Unready},
    job::{
        Cap, Environment, Guard, HoldFifo, Job, JobEnd, LogRead, Output, Ports, Readied, Readiness,
        Services, Start,
    },
    kill::DESCRIBED,
    lock::{
        Cards, Entry, GUARD_WAIT, Hold, Kind, Lock, LockDir, Urgent,
        cohort::{self, Sharing},
    },
    machine::Site,
    park::{self, Parking},
    platform::{Host, Platform as _},
    queue::Queue,
    series::Binding,
    session::{
        check_in::{self, CheckIn},
        ended::{Ended, Tally},
        laid::{Begins, Laid, Layout},
    },
    settings::{Settings, home},
    sink::Sink,
    status::Look,
    stop::{Stage, State, Stopper},
    tree::{Commands, Copier, KeptCopy, Mark, Stepping},
};
use dibs_format::{
    By, Event, Exit, HistoryLine, JobId, Mode,
    status::Ahead,
    wire::{Bundle, BundleMake, MaxFrom, Record},
};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::{Arc, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How long a job is given to stop once its cap has passed.
const JOB_GRACE: Duration = Duration::from_secs(30);
/// A job that waited this long is told when it got the lock.
const SAY_ACQUIRED_AFTER: u64 = 5;
/// How often a preemptible job looks again while a benchmark queued before it asks to pause.
const AHEAD_POLL: Duration = Duration::from_millis(100);
/// How often a benchmark tries `rw` again while it waits for the jobs that hold it, which is
/// how it learns that only preemptible ones it may freeze are left.
const BENCH_POLL: Duration = Duration::from_millis(50);
/// How often a `--low-priority` job looks for other jobs while it waits for none.
const IDLE_POLL: Duration = Duration::from_millis(500);
/// How long a job waits for a `--low-priority` one to stop, past which it is taken to be laying out
/// its tree, which it is not parked during.
const IDLE_STOP: Duration = Duration::from_secs(60);

/// How often a card benchmark kept from the gate looks whether a window opened for it to join.
const JOIN_RECHECK: Duration = Duration::from_secs(1);
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

/// The join guard's wait, cut to what is left of the call's own.
fn guard_wait(deadline: Option<Instant>) -> Duration {
    deadline.map_or(GUARD_WAIT, |d| {
        GUARD_WAIT.min(d.saturating_duration_since(Instant::now()))
    })
}

/// A call in the queue: its job's id, and the fifo a hold waits on.
struct Arrived {
    start: u64,
    /// When it arrived, for `--wait`, which whole seconds of `start` would round.
    since: Instant,
    /// When it arrived, in nanoseconds: the order a benchmark and a preemptible job are compared
    /// by, which a second cannot tell apart.
    order: u128,
    job: JobId,
    held: Option<HoldFifo>,
    /// The attempt this job is, held for as long as this runner serves it.
    _receipt: Option<Receipt>,
}

/// How waiting for another call's urgent job ended.
#[derive(PartialEq, Eq)]
enum Yielded {
    /// No urgent job was there.
    Free,
    /// It ended.
    Waited,
    GaveUp,
    Cancelled,
    /// Its machine closed while it was parked without the lock.
    Closed,
}

impl Yielded {
    /// The status a call that never ran its command ends with.
    fn status(&self) -> Option<i32> {
        match self {
            Yielded::Free | Yielded::Waited => None,
            Yielded::GaveUp => Some(Exit::Busy.status()),
            Yielded::Cancelled => Some(Exit::Cancelled.status()),
            Yielded::Closed => Some(Exit::Closed.status()),
        }
    }
}

/// What stops a running job before its end, to run on or again later: a benchmark that freezes
/// it, and, for a `--low-priority` job, its own handle on `rw`, let go of while it is parked.
struct Stops {
    frozen: Arc<Frozen>,
    idle_rw: Option<File>,
}

/// How waiting for an idle machine ended.
enum Idled {
    Idle,
    GaveUp,
    Cancelled,
}

/// How waiting for the lock ended.
enum Waited {
    Held,
    /// `--wait` passed while a benchmark queued ahead held the gate.
    GaveUpAtGate,
    GaveUpAtLock,
}

/// What a benchmark takes on a machine whose benchmarks lock cards.
enum Takes {
    Cards(Vec<String>),
    Machine,
}

/// The lock, taken. Fields drop in order, so a card benchmark stops handing out copies of the
/// lock before its own go.
struct Acquired {
    sharing: Option<Sharing>,
    cards: Option<Cards>,
    lock: Lock,
    at: u64,
    waited: u64,
    /// Preemptible: it holds `ahead` as well, and a benchmark queued before it freezes it.
    ahead: bool,
    /// Urgent: the machine's claim, which no other urgent job can take while this lives.
    claim: Option<File>,
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
    /// What a `--low-priority` job spent parked without the lock, which it did not run.
    pub idled: Duration,
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
        let preempts = self.preempts();
        let acquired = loop {
            if let Err(code) = self.until_open(&arrived) {
                return code;
            }
            if self.waits_idle() {
                match self.until_idle(self.wait_until(&arrived), None) {
                    Idled::Idle => {}
                    Idled::Cancelled => {
                        let mut state = self.at.stopper.state();
                        self.abandon(&arrived.job, &mut state);
                        return Exit::Cancelled.status();
                    }
                    Idled::GaveUp => {
                        let mut state = self.at.stopper.state();
                        self.abandon(&arrived.job, &mut state);
                        return Exit::Busy.status();
                    }
                }
            }
            let acquired = match (preempts, self.urgent(), self.card_claim()) {
                (_, Some(why), Some(Takes::Cards(pcis))) => {
                    match self.join_now(&arrived, &pcis, why) {
                        Ok(Some(acquired)) => Ok(acquired),
                        Ok(None) => self.acquire_urgent(&arrived, why),
                        Err(code) => Err(code),
                    }
                }
                (_, Some(why), _) => self.acquire_urgent(&arrived, why),
                (true, None, _) => self.acquire_ahead(&history, &arrived),
                (false, None, Some(takes)) => self.acquire_card(&history, &arrived, &takes),
                (false, None, None) => self.acquire(&history, &arrived),
            };
            let acquired = match acquired {
                Ok(acquired) => acquired,
                Err(code) => return code,
            };
            // Recorded as holding before it looks, so that a closing which saw no holder of its
            // has this call see the closure.
            let record = self.at.call.lock_record(acquired.at, &arrived.job);
            self.at.dir.hold(&record);
            if (Shutter { dir: self.at.dir }).shut().is_none() {
                break acquired;
            }
            // No window may list it between losing its admission and its holder record, and none
            // may count it live once its card is free for another, even if the guard is stuck.
            // The holder record goes first: without it, a window taken before the admission goes
            // counts this admission, which ends with it, rather than a holder that comes back.
            let guard = self.at.dir.join_guard(GUARD_WAIT);
            self.at.dir.write(Kind::Waiting, &record);
            let _ = fs::remove_file(self.at.dir.file("holder", self.at.call.pid));
            self.at.dir.leave_windows(self.at.call.pid);
            let _ = fs::remove_file(self.at.dir.file("urgent", self.at.call.pid));
            drop(acquired);
            drop(guard);
        };
        if !self.waits_idle() && !self.passes() {
            self.after_idle_parked();
        }
        // The machine as well as the cards: while the group winds down, shared work must not start.
        if let Some(cards) = &acquired.cards {
            cards
                .descriptors()
                .chain(acquired.lock.descriptors())
                .for_each(|fd| environment.hold(fd));
        }
        let mut state = self.at.stopper.state();
        let begun = match self.begin(&arrived, &acquired, &mut environment, &mut state) {
            Ok(begun) => begun,
            Err(code) => return code,
        };
        let frozen = Arc::new(Frozen::default());
        let preemption = preempts
            .then(|| acquired.lock.handles().ok())
            .flatten()
            .map(|(ahead, rw)| {
                Preemption::start(Watch {
                    dir: self.at.dir.clone(),
                    pid: self.at.call.pid,
                    arrived: arrived.order,
                    ahead,
                    rw,
                    stopper: Arc::clone(self.at.stopper),
                    frozen: Arc::clone(&frozen),
                })
            });
        let shut = closed::Watch::start(self.at.dir.clone(), Arc::clone(self.at.stopper));
        let check_in = request
            .check_in
            // A job its cap stops first wakes its caller then anyway.
            .filter(|every| {
                begun.log.is_some() && !request.watch.hold && (max == 0 || *every < max)
            })
            .map(|every| {
                CheckIn::start(
                    Duration::from_secs(every),
                    check_in::Job {
                        id: arrived.job.clone(),
                        dir: begun.dir.clone(),
                        pid: self.at.call.pid,
                    },
                    Arc::clone(self.at.stopper),
                    Arc::clone(&frozen),
                )
            });
        let idle_rw = self
            .idle()
            .then(|| acquired.lock.handles().ok())
            .flatten()
            .map(|(_, rw)| rw);
        let stops = Stops { frozen, idle_rw };
        let hosted = self.host(state, &arrived, &begun, environment, max, stops);
        if let Some(check_in) = check_in {
            check_in.end();
        }
        shut.end();
        if let Some(preemption) = preemption {
            preemption.end();
        }
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
        let since = Instant::now();
        let order = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let job = job_id(start, pid);
        if let Some(id) = request.attempt.as_ref().filter(|_| request.lookup) {
            let looked = Attempts::under(&self.at.machine.scratch).look(id);
            return Err(match looked {
                Ok(admitted) => {
                    let status = admitted.status();
                    self.at.sink.record(Record::Admitted(admitted));
                    status
                }
                Err(e) => {
                    self.at
                        .sink
                        .say(&format!("dibs: attempt {id} could not be looked up: {e}\n"));
                    Exit::Refused.status()
                }
            });
        }
        let receipt = match &request.attempt {
            Some(id) => match Attempts::under(&self.at.machine.scratch).admit(id, &job) {
                Ok(Ok(receipt)) => Some(receipt),
                Ok(Err(admitted)) => {
                    let status = admitted.status();
                    self.at.sink.record(Record::Admitted(admitted));
                    return Err(status);
                }
                Err(e) => {
                    self.at.sink.say(&format!(
                        "dibs: attempt {id} could not be admitted, so nothing ran: {e}\n"
                    ));
                    return Err(Exit::Refused.status());
                }
            },
            None => None,
        };
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
        let claim = self.card_claim();
        let machine = matches!(claim, Some(Takes::Machine));
        // A whole-machine benchmark's record and the card benchmarks it lets ahead appear at once;
        // without the guard it lists none, and lets none ahead.
        let within = guard_wait(request.wait.map(|wait| since + Duration::from_secs(wait)));
        let window = machine.then(|| self.at.dir.join_guard(within));
        // Before the record, so nobody reads it as a benchmark of the whole machine.
        match &claim {
            Some(Takes::Cards(pcis)) => {
                self.at.dir.mark_cards(pid, pcis);
                self.at.dir.mark_arrived(pid, order);
            }
            Some(Takes::Machine) => {
                self.at.dir.forget_window(pid);
                if let Some(Ok(_)) = &window {
                    self.at.dir.open_window(pid);
                }
                self.at.dir.mark_arrived(pid, order);
            }
            None => {}
        }
        // Beside the lock record rather than in it: a runner reading a record with a field it
        // does not know drops the record, and with it a holder its benchmark must not run beside.
        // Before it, so the first status to see the call sees them too.
        if let Some(subagent) = &self.at.call.subagent {
            let _ = fs::write(self.at.dir.file("subagent", pid), format!("{subagent}\n"));
        }
        if request.command.len() > ONE_LINE && request.mode != Mode::Gc {
            let _ = fs::write(self.at.dir.file("command", pid), &request.command);
        }
        if let Some(user) = &self.at.call.user {
            let _ = fs::write(self.at.dir.file("user", pid), format!("{user}\n"));
        }
        if self.waits_idle() {
            self.at.dir.mark_idle(pid, self.idle());
            self.at
                .dir
                .mark_quiet(pid, None, self.at.settings.idle_settle);
        }
        // Before any wait, a closed machine's included, so the queue shows it from the start.
        if let (Mode::Shared | Mode::Bench, Some(why)) = (request.mode, &request.urgent) {
            self.at.dir.mark_pressing(pid, why);
        }
        if self.passes() {
            self.at.dir.mark_passing(pid);
        }
        self.at
            .dir
            .write(Kind::Waiting, &self.at.call.lock_record(start, &job));
        drop(window);
        if let Some(batch) = request.batch.as_deref().filter(|b| !b.is_empty()) {
            let _ = fs::write(self.at.dir.file("batch", pid), format!("{batch}\n"));
        }
        let mut line = self.at.call.log_line(Event::Arrived);
        line.job = Some(job.clone());
        self.at.journal().write(&line);
        state.set_stage(Stage::Queued);
        state.job = Some(job.clone());
        Ok(Arrived {
            start,
            since,
            order,
            job,
            held,
            _receipt: receipt,
        })
    }

    /// Waits while its machine is closed to it, when the call said to; refuses it otherwise.
    fn until_open(&self, arrived: &Arrived) -> Result<(), i32> {
        let mut told = false;
        while let Some(closure) = (Shutter { dir: self.at.dir }).shut() {
            if !self.at.call.request.when_open {
                self.at.sink.say(&format!(
                    "{}  It closed while this call was queued, which leaves the queue.\n",
                    closed::said(&closure)
                ));
                let mut state = self.at.stopper.state();
                self.abandon(&arrived.job, &mut state);
                return Err(Exit::Closed.status());
            }
            if !told {
                self.at.sink.say(&format!(
                    "{}  Queued for when it opens (--when-open).\n",
                    closed::said(&closure)
                ));
                told = true;
            }
            std::thread::sleep(closed::POLL);
        }
        Ok(())
    }

    /// Why the call is urgent, when it is a benchmark that is: a shared job marked urgent only
    /// goes around the gate.
    fn urgent(&self) -> Option<&str> {
        (self.at.call.mode() == Mode::Bench)
            .then_some(self.at.call.request.urgent.as_deref())
            .flatten()
    }

    /// A shared job whose command can be stopped and run again from the start: nothing held from
    /// elsewhere and no server to bring back.
    fn parkable(&self) -> bool {
        let request = &self.at.call.request;
        self.at.call.mode() == Mode::Shared && !request.watch.hold && request.services.is_empty()
    }

    /// Takes the machine's claim, then waits for every other holder to park or end, without the
    /// gate. One urgent job at a time: a second is refused.
    fn acquire_urgent(&self, arrived: &Arrived, why: &str) -> Result<Acquired, i32> {
        let request = &self.at.call.request;
        let pid = self.at.call.pid;
        let refused = |e: &io::Error| {
            self.at.sink.say(&format!(
                "dibs: the lock in {} could not be taken: {e}. Nothing was run.\n",
                self.at.dir.path.display()
            ));
            self.at.dir.clear(pid);
            Exit::NoLock.status()
        };
        let lock = Lock::open(self.at.dir).map_err(|e| refused(&e))?;
        let claim = match self.at.dir.claim_urgent() {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                let first = self.at.dir.urgent_besides(pid).map_or_else(
                    || "another urgent job".to_string(),
                    |u| format!("the urgent job {} from {}: {}", u.job, u.agent, u.why),
                );
                self.at.sink.say(&format!(
                    "dibs: {} already runs {first}. One urgent job at a time, so this one did not run.\n",
                    self.at.machine.host
                ));
                let mut state = self.at.stopper.state();
                self.abandon(&arrived.job, &mut state);
                return Err(Exit::Refused.status());
            }
            Err(e) => return Err(refused(&e)),
        };
        self.at
            .dir
            .mark_urgent(&Urgent {
                pid,
                job: arrived.job.clone(),
                agent: self.at.call.agent.clone(),
                why: why.to_string(),
            })
            .map_err(|e| refused(&e))?;
        let deadline = self.wait_until(arrived);
        let mut told = false;
        loop {
            let running = self.at.dir.unparked(pid, &arrived.job);
            if running.is_empty()
                && self
                    .urgent_rw(&lock, &arrived.job)
                    .map_err(|e| refused(&e))?
            {
                break;
            }
            if !told {
                let labels: Vec<String> = running.iter().map(|h| h.label.to_string()).collect();
                let what = match labels.is_empty() {
                    true => "the lock, which a process with no record holds".to_string(),
                    false => format!("{} to park or end: {}", labels.len(), labels.join(", ")),
                };
                self.at.sink.say(&format!(
                    "dibs: urgent, so the shared jobs here are parked and run again after it. Waiting for {what}\n"
                ));
                told = true;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                let wait = request.wait.unwrap_or_default();
                self.at.sink.say(&format!(
                    "dibs: still busy after {wait}s, gave up: what cannot be parked still runs.\n"
                ));
                let mut state = self.at.stopper.state();
                self.abandon(&arrived.job, &mut state);
                return Err(Exit::Busy.status());
            }
            std::thread::sleep(BENCH_POLL);
        }
        let now = Moment::epoch_now();
        Ok(Acquired {
            sharing: None,
            cards: None,
            lock,
            at: now,
            waited: now.saturating_sub(arrived.start),
            ahead: false,
            claim: Some(claim),
        })
    }

    /// `rw` shared with no orphan beside it, which keeps out a benchmark from a runner that knows
    /// nothing of the claim; or held exclusive by a benchmark parked for this job, which runs
    /// nothing. An orphan is waited for, where the system can say who holds a lock.
    fn urgent_rw(&self, lock: &Lock, job: &JobId) -> io::Result<bool> {
        let held = lock.take(Hold::Shared, Some(Instant::now()))?;
        if !self.at.dir.takers(self.at.call.pid).orphans.is_empty() {
            return Ok(false);
        }
        Ok(held || self.at.dir.bench_parked_for(self.at.call.pid, job))
    }

    /// Runs nothing, with `parked.<pid>` written, while another call's urgent job has the machine.
    /// `until` is the call's `--wait`, which counts only before its command first starts.
    fn yield_to_urgent(
        &self,
        mut state: MutexGuard<'a, State>,
        until: Option<Instant>,
    ) -> (MutexGuard<'a, State>, Yielded) {
        let pid = self.at.call.pid;
        let Some(urgent) = self.at.dir.urgent_besides(pid) else {
            return (state, Yielded::Free);
        };
        let was = state.stage;
        state.set_stage(Stage::Parked);
        drop(state);
        self.at.dir.park(pid, &urgent.job);
        self.at.sink.say(&format!(
            "dibs: parked for the urgent job {} from {}: {}. It runs again from the start once that ends.\n",
            urgent.job, urgent.agent, urgent.why
        ));
        let yielded = loop {
            if self.cancelled() {
                break Yielded::Cancelled;
            }
            let Some(now) = self.at.dir.urgent_besides(pid) else {
                break Yielded::Waited;
            };
            // Said again each look: an urgent job that follows the last waits for its own job's
            // name, and a write the disk refused once may go through.
            if self.at.dir.parked_for(pid).as_ref() != Some(&now.job) {
                self.at.dir.park(pid, &now.job);
            }
            if until.is_some_and(|until| Instant::now() >= until) {
                let wait = self.at.call.request.wait.unwrap_or_default();
                self.at.sink.say(&format!(
                    "dibs: still busy after {wait}s, gave up: the urgent job still runs.\n"
                ));
                break Yielded::GaveUp;
            }
            std::thread::sleep(park::POLL);
        };
        self.at.dir.unpark(pid);
        let mut state = self.at.stopper.state();
        state.set_stage(was);
        (state, yielded)
    }

    /// A shared job run only while nothing else here runs or waits, and parked for anything that
    /// comes; urgent, it parks others instead.
    fn idle(&self) -> bool {
        let request = &self.at.call.request;
        request.low_priority && request.urgent.is_none() && self.parkable()
    }

    /// A `--low-priority` call that waits for an idle machine to start: a parkable job, and the
    /// transfer that sends a recipe's tree, which is short and is never parked once it runs.
    fn waits_idle(&self) -> bool {
        let request = &self.at.call.request;
        self.idle()
            || (request.low_priority
                && request.urgent.is_none()
                && self.at.call.mode() == Mode::Rsh)
    }

    /// Waits until no other job has held or waited here for the machine's settle time, saying
    /// once what it waits for; `said` is what it said when it was parked, so it says no more.
    fn until_idle(&self, until: Option<Instant>, said: Option<&str>) -> Idled {
        let pid = self.at.call.pid;
        let settle = Duration::from_secs(self.at.settings.idle_settle);
        let mut told = said.is_some();
        let mut quiet: Option<Instant> = None;
        self.at.dir.mark_quiet(pid, None, settle.as_secs());
        loop {
            if self.cancelled() {
                return Idled::Cancelled;
            }
            match self.at.dir.busy_besides(pid) {
                Some(busy) => {
                    if quiet.take().is_some() {
                        self.at.dir.mark_quiet(pid, None, settle.as_secs());
                    }
                    if !told {
                        self.at.sink.say(&format!(
                            "dibs: --low-priority, so it waits until nothing else runs or waits here: {} from {} is there.\n",
                            busy.label, busy.agent
                        ));
                        told = true;
                    }
                }
                None => {
                    let since = *quiet.get_or_insert_with(|| {
                        self.at
                            .dir
                            .mark_quiet(pid, Some(Moment::epoch_now()), settle.as_secs());
                        Instant::now()
                    });
                    if since.elapsed() >= settle {
                        self.at.dir.clear_quiet(pid);
                        return Idled::Idle;
                    }
                }
            }
            if until.is_some_and(|until| Instant::now() >= until) {
                let wait = self.at.call.request.wait.unwrap_or_default();
                self.at.sink.say(&format!(
                    "dibs: still busy after {wait}s, gave up: --low-priority waits for an idle machine.\n"
                ));
                return Idled::GaveUp;
            }
            std::thread::sleep(IDLE_POLL);
        }
    }

    /// Parked, or about to start, while another job holds or waits here: lets go of `rw`, waits
    /// until the machine has been idle a while, and takes `rw` again, waiting as before if that
    /// fails. Its record says it waits meanwhile, which is what status and a benchmark read.
    fn yield_while_busy(
        &self,
        state: MutexGuard<'a, State>,
        rw: &File,
        arrived: &Arrived,
        until: Option<Instant>,
        parked: bool,
    ) -> (MutexGuard<'a, State>, Yielded) {
        let pid = self.at.call.pid;
        // Parked for a job that has already gone, it still waits the machine's settle time.
        let busy = match (self.at.dir.busy_besides(pid), parked) {
            (Some(busy), _) => format!("{} from {}", busy.label, busy.agent),
            (None, true) => "a job that has since ended".to_string(),
            (None, false) => return (state, Yielded::Free),
        };
        let was = state.stage;
        let mut state = state;
        state.set_stage(Stage::Parked);
        drop(state);
        let waiting = self.at.call.lock_record(arrived.start, &arrived.job);
        self.at.dir.write(Kind::Waiting, &waiting);
        let _ = fs::remove_file(self.at.dir.file("holder", pid));
        let _ = rw.unlock();
        self.at.dir.set_deadline(pid, None);
        let said = format!(
            "dibs: parked for {busy}, since --low-priority makes way for any other job. It runs again from the start once the machine is idle.\n"
        );
        self.at.sink.say(&said);
        let yielded = loop {
            match self.until_idle(until, Some(&said)) {
                Idled::Idle => {}
                Idled::Cancelled => break Yielded::Cancelled,
                Idled::GaveUp => break Yielded::GaveUp,
            }
            if let Some(closure) = (Shutter { dir: self.at.dir }).shut() {
                if !self.at.call.request.when_open {
                    self.at.sink.say(&format!(
                        "{}  It closed while this call was parked, which leaves the queue.\n",
                        closed::said(&closure)
                    ));
                    break Yielded::Closed;
                }
                std::thread::sleep(closed::POLL);
                continue;
            }
            if matches!(Lock::try_hold(rw, Hold::Shared), Ok(true)) {
                // Holding before it looks, as at its first admission: a job or a closing that
                // comes meanwhile sees it, and it sees them.
                let record = self.at.call.lock_record(Moment::epoch_now(), &arrived.job);
                self.at.dir.hold(&record);
                if self.at.dir.busy_besides(pid).is_none()
                    && (Shutter { dir: self.at.dir }).shut().is_none()
                {
                    break Yielded::Waited;
                }
                self.at.dir.write(Kind::Waiting, &waiting);
                let _ = fs::remove_file(self.at.dir.file("holder", pid));
                let _ = rw.unlock();
            }
        };
        if yielded == Yielded::Waited {
            self.at.dir.mark_stopped(pid, None);
            self.at
                .sink
                .say("dibs: the machine is idle again, so it runs from the start.\n");
        }
        let mut state = self.at.stopper.state();
        state.set_stage(was);
        (state, yielded)
    }

    /// A copy that lays out no tree: no job runs from it, so a `--low-priority` job is not parked
    /// for it, and it does not wait for one to stop.
    fn passes(&self) -> bool {
        self.at.call.mode() == Mode::Rsh && self.at.call.request.tree.is_none()
    }

    /// Lets a `--low-priority` job running here stop first, which it does once it sees this one:
    /// until then its command may still hold memory or a card this job needs.
    fn after_idle_parked(&self) {
        let until = Instant::now() + IDLE_STOP;
        while self.at.dir.idle_holding(self.at.call.pid) && Instant::now() < until {
            std::thread::sleep(park::POLL);
        }
    }

    /// The call is a step of a batch cancelled since it arrived.
    fn cancelled(&self) -> bool {
        self.at
            .call
            .batch_id()
            .is_some_and(|b| self.at.dir.cancelled(b))
    }

    /// When `--wait` runs out, counted from the call's arrival.
    fn wait_until(&self, arrived: &Arrived) -> Option<Instant> {
        self.at
            .call
            .request
            .wait
            .map(|wait| arrived.since + Duration::from_secs(wait))
    }

    /// A job that only compiles may be frozen, which nothing that holds a card, serves, or is
    /// held from elsewhere can be.
    fn preempts(&self) -> bool {
        let request = &self.at.call.request;
        request.preempt
            && !self.idle()
            && self.at.call.mode() == Mode::Shared
            && !request.watch.hold
            && request.services.is_empty()
            && request.ports.is_empty()
    }

    /// Takes `ahead` and then `rw` shared without passing the gate: such a job starts at once
    /// unless a benchmark runs or one queued before it asks it to freeze. Holding `rw` keeps a
    /// benchmark from a runner that knows nothing of `ahead` from starting beside it.
    fn acquire_ahead(&self, history: &History, arrived: &Arrived) -> Result<Acquired, i32> {
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
        let deadline = request
            .wait
            .map(|wait| Instant::now() + Duration::from_secs(wait));
        let mut told = false;
        let taken = loop {
            if self.at.dir.pausing_before(arrived.order).is_none() {
                if !told && !lock.free_ahead() {
                    self.at.sink.say(&queue.line());
                    told = true;
                }
                match lock.take_ahead(Hold::Shared, deadline) {
                    Ok(true) if self.at.dir.pausing_before(arrived.order).is_none() => {
                        match lock.take(Hold::Shared, Some(Instant::now())) {
                            Ok(true) => break Ok(true),
                            Ok(false) => lock.let_ahead_go(),
                            Err(e) => break Err(e),
                        }
                    }
                    Ok(true) => lock.let_ahead_go(),
                    Ok(false) => break Ok(false),
                    Err(e) => break Err(e),
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                break Ok(false);
            }
            std::thread::sleep(AHEAD_POLL);
        };
        match taken {
            Ok(true) => {}
            Ok(false) => {
                let wait = request.wait.unwrap_or_default();
                self.at
                    .sink
                    .say(&format!("dibs: still busy after {wait}s, gave up.\n"));
                let mut state = self.at.stopper.state();
                self.abandon(&arrived.job, &mut state);
                return Err(Exit::Busy.status());
            }
            Err(e) => {
                self.at.sink.say(&format!(
                    "dibs: the lock in {} could not be taken: {e}. Nothing was run.\n",
                    self.at.dir.path.display()
                ));
                self.at.dir.clear(pid);
                return Err(Exit::NoLock.status());
            }
        }
        let now = Moment::epoch_now();
        Ok(Acquired {
            sharing: None,
            cards: None,
            lock,
            at: now,
            waited: now.saturating_sub(arrived.start),
            ahead: true,
            claim: None,
        })
    }

    /// Waits its turn: through the gate unless it may go around, then for the lock, within
    /// `--wait` when there is one. A benchmark then has the jobs started ahead of it frozen, and
    /// what they wrote written out, before it runs.
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
        // Waiting for an idle machine has already spent some of it.
        let deadline = match self.waits_idle() {
            true => self.wait_until(arrived),
            false => request
                .wait
                .map(|wait| Instant::now() + Duration::from_secs(wait)),
        };
        let block = lock.free(Hold::Shared) && !lock.free(Hold::Exclusive);
        let urgent = request.urgent.is_some() && self.at.call.mode() == Mode::Shared;
        let passed = match urgent || queue.may_bypass(self.at.settings, block) {
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
                let taken = match hold {
                    Hold::Exclusive => self.take_for_bench(&lock, arrived, deadline),
                    Hold::Shared => lock.take(hold, deadline),
                };
                taken.map(|taken| match taken {
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
        let gave_up = match (gave_up, hold) {
            (None, Hold::Exclusive) => match self.pause_ahead(&lock, arrived, deadline) {
                Ok(true) => None,
                Ok(false) => Some(format!(
                    "dibs: still busy after {wait}s, gave up: a job queued before it still runs.\n"
                )),
                Err(e) => {
                    self.at.sink.say(&format!(
                        "dibs: the lock in {} could not be taken: {e}. Nothing was run.\n",
                        self.at.dir.path.display()
                    ));
                    self.at.dir.clear(pid);
                    return Err(Exit::NoLock.status());
                }
            },
            (gave_up, _) => gave_up,
        };
        if let Some(said) = gave_up {
            self.at.sink.say(&said);
            self.at.sink.say(&shown());
            let mut state = self.at.stopper.state();
            self.abandon(&arrived.job, &mut state);
            return Err(Exit::Busy.status());
        }
        let now = Moment::epoch_now();
        Ok(Acquired {
            sharing: None,
            cards: None,
            lock,
            at: now,
            waited: now.saturating_sub(arrived.start),
            ahead: false,
            claim: None,
        })
    }

    /// What a benchmark takes on a machine whose benchmarks lock cards. A hold or a job with
    /// servers takes the whole machine, since nothing confines what it starts to a card.
    fn card_claim(&self) -> Option<Takes> {
        let request = &self.at.call.request;
        if !self.at.settings.card_benchmarks || self.at.call.mode() != Mode::Bench {
            return None;
        }
        let pcis: Option<Vec<String>> = request
            .card
            .iter()
            .chain(&request.also)
            .map(|card| card.pci.clone())
            .collect();
        let named = request.card.iter().chain(&request.also).count();
        let confined = request
            .card
            .iter()
            .chain(&request.also)
            .all(|card| Environment::confines(card, named));
        match pcis {
            Some(pcis)
                if !pcis.is_empty()
                    && confined
                    && !request.watch.hold
                    && request.services.is_empty() =>
            {
                Some(Takes::Cards(pcis))
            }
            _ => Some(Takes::Machine),
        }
    }

    fn no_lock(&self, e: &io::Error) -> i32 {
        self.at.sink.say(&format!(
            "dibs: the lock in {} could not be taken: {e}. Nothing was run.\n",
            self.at.dir.path.display()
        ));
        self.at.dir.clear(self.at.call.pid);
        Exit::NoLock.status()
    }

    /// A card benchmark joins the ones running when its cards are free, and takes the machine as
    /// a benchmark does when none runs. It never waits holding part of what it needs, so one
    /// waiting on a busy card keeps nobody from an idle one. A benchmark of the whole machine
    /// waits here too, so that one queued for a card before it still goes first.
    fn acquire_card(
        &self,
        history: &History,
        arrived: &Arrived,
        takes: &Takes,
    ) -> Result<Acquired, i32> {
        let request = &self.at.call.request;
        let pid = self.at.call.pid;
        let dir = self.at.dir;
        let no_lock = |e: io::Error| self.no_lock(&e);
        let pcis = match takes {
            Takes::Cards(pcis) => Some(pcis.as_slice()),
            Takes::Machine => None,
        };
        let mut lock = Lock::open(dir).map_err(no_lock)?;
        let deadline = self.wait_until(arrived);
        let (mut at_gate, mut told, mut asked, mut last_look) = (false, false, false, false);
        let held = loop {
            // A whole-machine benchmark keeps the gate while it waits, so a card benchmark it lets
            // go ahead joins the running ones without it, as an urgent one does.
            let open = pcis.is_some() && dir.open_to_join(pid, arrived.order);
            if !at_gate
                && open
                && let Some(pcis) = pcis
                && !dir.bench_first(pid, arrived.order, Some(pcis))
                && dir.urgent_besides(pid).is_none()
                && let Some(cards) = self.join(&mut lock, pcis, deadline)?
            {
                // Its copies of the machine's locks are closed, never unlocked, which would free
                // them for every member.
                if dir
                    .enter_windows(
                        pid,
                        arrived.order,
                        pcis,
                        Entry::JoinsAhead,
                        guard_wait(deadline),
                    )
                    .map_err(no_lock)?
                {
                    break Some((Some(cards), true));
                }
                drop(cards);
                lock = Lock::open(dir).map_err(no_lock)?;
            }
            let mut gated_out = false;
            if !at_gate {
                let attempt = match pcis.is_some() {
                    true => Some(deadline.map_or(Instant::now() + JOIN_RECHECK, |d| {
                        d.min(Instant::now() + JOIN_RECHECK)
                    })),
                    false => deadline,
                };
                match lock.pass_gate(attempt).map_err(no_lock)? {
                    true => at_gate = true,
                    false if pcis.is_some() && deadline.is_none_or(|d| Instant::now() < d) => {
                        gated_out = true;
                    }
                    // Out of time at the gate: one more look whether a group opened to join.
                    false if pcis.is_some() && !last_look => {
                        last_look = true;
                        continue;
                    }
                    false => break None,
                }
            }
            let others_first =
                dir.bench_first(pid, arrived.order, pcis) || dir.urgent_besides(pid).is_some();
            if at_gate && !others_first {
                if let Some(pcis) = pcis
                    && let Some(cards) = self.join(&mut lock, pcis, deadline)?
                {
                    if dir
                        .enter_windows(pid, arrived.order, pcis, Entry::Joins, guard_wait(deadline))
                        .map_err(no_lock)?
                    {
                        lock.leave_gate();
                        break Some((Some(cards), true));
                    }
                    drop(cards);
                    lock = Lock::open(dir).map_err(no_lock)?;
                    at_gate = false;
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        break None;
                    }
                    std::thread::sleep(BENCH_POLL);
                    continue;
                }
                if lock
                    .take(Hold::Exclusive, Some(Instant::now()))
                    .map_err(no_lock)?
                {
                    let taken = match pcis {
                        Some(pcis) => Cards::take_now(dir, pcis).map_err(no_lock)?.map(Some),
                        None => Some(None),
                    };
                    let entered = match (&taken, pcis) {
                        (Some(Some(_)), Some(p)) => dir
                            .enter_windows(
                                pid,
                                arrived.order,
                                p,
                                Entry::Starts,
                                guard_wait(deadline),
                            )
                            .map_err(no_lock)?,
                        _ => true,
                    };
                    match taken {
                        Some(cards) if entered => {
                            lock.leave_gate();
                            break Some((cards, false));
                        }
                        _ => lock.let_go(),
                    }
                } else if !asked && dir.only_later_preemptible(arrived.order) {
                    dir.ask_to_pause(pid, arrived.order, &arrived.job);
                    asked = true;
                }
            }
            if !told {
                self.at.sink.say(&match pcis {
                    Some(pcis) => format!(
                        "dibs: waiting for {} and the benchmarks ahead of it.\n",
                        pcis.join(" ")
                    ),
                    None => "dibs: waiting for what runs here and the benchmarks ahead of it.\n"
                        .to_string(),
                });
                if request.verbose {
                    let look = Look {
                        machine: self.at.machine,
                        dir,
                        history,
                        settings: self.at.settings,
                        asking: pid,
                    };
                    self.at
                        .sink
                        .say(&look.text(&look.status(true), request.tty));
                }
                told = true;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                if pcis.is_some() && !last_look {
                    last_look = true;
                    continue;
                }
                break None;
            }
            // Waiting out shared work keeps the gate, as a benchmark does, so none starts
            // meanwhile; waiting on a card or on a benchmark ahead lets others reach theirs.
            let card_running = dir
                .records(Kind::Holder)
                .iter()
                .any(|h| h.pid != pid && dir.cards_of(h.pid).is_some());
            if at_gate && (others_first || (pcis.is_some() && card_running)) {
                lock.leave_gate();
                at_gate = false;
            }
            if !gated_out {
                std::thread::sleep(BENCH_POLL);
            }
        };
        let Some((cards, adopted)) = held else {
            lock.leave_gate();
            let _ = fs::remove_file(dir.file("pause", pid));
            let wait = request.wait.unwrap_or_default();
            self.at
                .sink
                .say(&format!("dibs: still busy after {wait}s, gave up.\n"));
            let mut state = self.at.stopper.state();
            self.abandon(&arrived.job, &mut state);
            return Err(Exit::Busy.status());
        };
        if !adopted
            && !self
                .pause_ahead(&lock, arrived, deadline)
                .map_err(no_lock)?
        {
            let wait = request.wait.unwrap_or_default();
            self.at.sink.say(&format!(
                "dibs: still busy after {wait}s, gave up: a job queued before it still runs.\n"
            ));
            let mut state = self.at.stopper.state();
            self.abandon(&arrived.job, &mut state);
            return Err(Exit::Busy.status());
        }
        Ok(self.with_cards(lock, cards, arrived))
    }

    /// An urgent benchmark whose cards are free beside the ones running joins them at once,
    /// ahead of any queue, rather than wait for the machine to empty. It takes the machine's one
    /// urgent claim all the same.
    fn join_now(
        &self,
        arrived: &Arrived,
        pcis: &[String],
        why: &str,
    ) -> Result<Option<Acquired>, i32> {
        let Ok(Some(claim)) = self.at.dir.claim_urgent() else {
            return Ok(None);
        };
        // Around the gate, since a benchmark of the whole machine may wait there for the very
        // benchmarks this one joins.
        let mut lock = Lock::open(self.at.dir).map_err(|e| self.no_lock(&e))?;
        let deadline = self.wait_until(arrived);
        let Some(cards) = self.join(&mut lock, pcis, deadline)? else {
            return Ok(None);
        };
        let within = guard_wait(deadline);
        let guard = match self.at.dir.join_guard(within) {
            Ok(guard) => guard,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && within < GUARD_WAIT => {
                return Ok(None);
            }
            Err(e) => return Err(self.no_lock(&e)),
        };
        self.at
            .dir
            .count_in(self.at.call.pid, arrived.order)
            .map_err(|e| self.no_lock(&e))?;
        let marked = self.at.dir.mark_urgent(&Urgent {
            pid: self.at.call.pid,
            job: arrived.job.clone(),
            agent: self.at.call.agent.clone(),
            why: why.to_string(),
        });
        if marked.is_err() {
            self.at.dir.leave_windows(self.at.call.pid);
        }
        drop(guard);
        marked.map_err(|e| self.no_lock(&e))?;
        let mut acquired = self.with_cards(lock, Some(cards), arrived);
        acquired.claim = Some(claim);
        Ok(Some(acquired))
    }

    /// The cards, with copies of the running benchmarks' hold on the machine.
    fn join(
        &self,
        lock: &mut Lock,
        pcis: &[String],
        deadline: Option<Instant>,
    ) -> Result<Option<Cards>, i32> {
        let Some((rw, ahead)) = cohort::join(self.at.dir, self.at.call.pid, deadline) else {
            return Ok(None);
        };
        let cards = Cards::take_now(self.at.dir, pcis).map_err(|e| self.no_lock(&e))?;
        if cards.is_some() {
            lock.adopt(rw, ahead);
        }
        Ok(cards)
    }

    /// Held, and offering its hold on the machine to the card benchmarks that come after it.
    fn with_cards(&self, lock: Lock, cards: Option<Cards>, arrived: &Arrived) -> Acquired {
        let shared = cards.is_some().then(|| {
            lock.handles()
                .and_then(|(ahead, rw)| Sharing::start(self.at.dir, self.at.call.pid, rw, ahead))
        });
        let sharing = match shared {
            Some(Ok(sharing)) => Some(sharing),
            Some(Err(e)) => {
                self.at.sink.say(&format!(
                    "dibs: benchmarks on other cards cannot run beside this one, which could not offer them its hold on the machine: {e}\n"
                ));
                None
            }
            None => None,
        };
        let now = Moment::epoch_now();
        Acquired {
            sharing,
            cards,
            lock,
            at: now,
            waited: now.saturating_sub(arrived.start),
            ahead: false,
            claim: None,
        }
    }

    /// `rw`, exclusive. Once only preemptible jobs queued after this benchmark hold it, they are
    /// asked to freeze and let it go; one queued before it is waited for like any shared job.
    fn take_for_bench(
        &self,
        lock: &Lock,
        arrived: &Arrived,
        deadline: Option<Instant>,
    ) -> io::Result<bool> {
        let pid = self.at.call.pid;
        let mut asked = false;
        loop {
            if lock.take(Hold::Exclusive, Some(Instant::now()))? {
                return Ok(true);
            }
            if !asked && self.at.dir.only_later_preemptible(arrived.order) {
                self.at.dir.ask_to_pause(pid, arrived.order, &arrived.job);
                asked = true;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                let _ = fs::remove_file(self.at.dir.file("pause", pid));
                return Ok(false);
            }
            std::thread::sleep(BENCH_POLL);
        }
    }

    /// Takes `ahead` once each preemptible job queued after this benchmark has frozen, then
    /// writes out what they wrote. False when the deadline passes first.
    fn pause_ahead(
        &self,
        lock: &Lock,
        arrived: &Arrived,
        deadline: Option<Instant>,
    ) -> io::Result<bool> {
        let pid = self.at.call.pid;
        self.at.dir.ask_to_pause(pid, arrived.order, &arrived.job);
        let taken = lock.take_ahead(Hold::Exclusive, deadline);
        let _ = fs::remove_file(self.at.dir.file("pause", pid));
        if matches!(taken, Ok(true)) {
            Host::flush(&self.at.machine.scratch);
        }
        taken
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
        // Marked first, so no status shows a preemptible holder as an ordinary one.
        if acquired.ahead {
            self.at
                .dir
                .mark_ahead(self.at.call.pid, arrived.order, &Ahead::Running);
        }
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
        if let Some(id) = &self.at.call.request.attempt
            && let Err(e) = Attempts::under(&self.at.machine.scratch).started(id)
        {
            self.at.sink.say(&format!(
                "dibs: attempt {id} could not be marked started, so it did not run: {e}\n"
            ));
            self.abandon(job, state);
            return Err(Exit::Setup.status());
        }
        let dir = self.at.machine.jobs().join(job.as_str());
        let kept = match transfer {
            true => Ok(()),
            false => fs::create_dir_all(&dir)
                .and_then(|()| Run::write_command(&dir.join("cmd"), &self.at.call.work)),
        };
        // A sweep is what makes the room, so it runs without a log rather than not at all.
        if let Err(e) = &kept
            && no_room(e)
            && mode != Mode::Gc
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

    /// When the job's cap kills it, in seconds since the epoch, for status to say.
    fn show_deadline(&self, deadline: Deadline) {
        let at = deadline
            .left()
            .and_then(|left| SystemTime::now().checked_add(left))
            .and_then(|at| at.duration_since(UNIX_EPOCH).ok());
        self.at
            .dir
            .set_deadline(self.at.call.pid, at.map(|at| at.as_secs_f64()));
    }

    /// The ports and services the job asked for, then the job itself, to its end.
    fn host(
        &self,
        mut state: MutexGuard<'a, State>,
        arrived: &Arrived,
        begun: &Begun,
        mut environment: Environment,
        max: u64,
        stops: Stops,
    ) -> Hosted {
        let Stops { frozen, idle_rw } = stops;
        let request = &self.at.call.request;
        let pid = self.at.call.pid;
        let output = match (&begun.log, request.stream) {
            _ if self.at.call.mode() == Mode::Rsh => Output::Through,
            (Some(log), true) => Output::Stream(log),
            (Some(log), false) => Output::Log(log),
            (None, _) => Output::Caller,
        };
        let (held, yielded) = match idle_rw {
            Some(_) => (state, Yielded::Free),
            None => self.yield_to_urgent(state, self.wait_until(arrived)),
        };
        state = held;
        if let Some(status) = yielded.status() {
            drop(state);
            begun.touch_log();
            return Hosted {
                status,
                ports: Ports {
                    picked: Vec::new(),
                    unpicked: Vec::new(),
                },
                services: None,
                failed: false,
                laid: Some(By::Dibs),
                overran: false,
                idled: Duration::ZERO,
            };
        }
        let mut deadline = Deadline::after((max > 0).then(|| Duration::from_secs(max)));
        self.show_deadline(deadline);
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
            idled: Duration::ZERO,
        };
        // Checked before the tree is laid out, since the image names the target it builds in.
        let image = match request.image.as_deref().map(Image::parse) {
            None => None,
            Some(Ok(image)) => Some(image),
            Some(Err(why)) => {
                drop(state);
                self.at.sink.say(&why);
                begun.touch_log();
                hosted.laid = Some(By::Dibs);
                hosted.status = Exit::Setup.status();
                return hosted;
            }
        };
        let layout = Layout::new(self.at, output);
        let mut spot = None;
        // Held to the end of the job: a transfer's lock on the tree it copies into.
        let mut _copying = None;
        let mut transfer = false;
        if let Some(tree) = &request.tree
            && hosted.ports.complete()
        {
            state.set_stage(Stage::Preparing(None));
            drop(state);
            let laid = layout.lay_out(tree, &mut environment, deadline);
            state = self.at.stopper.state();
            match laid {
                Laid::Run(laid) => spot = laid,
                Laid::Transfer(turn) => {
                    _copying = turn;
                    transfer = true;
                }
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
        // A hold's command only waits on a fifo in the lock directory, which no image binds, and
        // a transfer carries the image only to name the target it lands beside.
        let built_in = image
            .as_ref()
            .filter(|_| request.mode == Mode::Shared && !request.watch.hold && !transfer);
        let sandbox = match built_in {
            None => None,
            Some(_) if request.tree.is_none() => {
                drop(state);
                self.at.sink.say(
                    "dibs: building in an image needs a tree dibs lays out, from a recipe or `dibs shell <repo>@<ref>`, since only it gets a target of the image's own\n",
                );
                begun.touch_log();
                hosted.laid = Some(By::Dibs);
                hosted.status = Exit::Setup.status();
                return hosted;
            }
            Some(image) => {
                drop(state);
                let sandbox = self.sandbox(image, &environment, deadline);
                state = self.at.stopper.state();
                match sandbox {
                    Ok(sandbox) => Some(sandbox),
                    Err(unready) => {
                        drop(state);
                        self.at.sink.say(&unready.why);
                        begun.touch_log();
                        hosted.laid = Some(By::Dibs);
                        hosted.overran = unready.overran;
                        hosted.status = match unready.overran {
                            true => Exit::Overran.status(),
                            false => Exit::Setup.status(),
                        };
                        return hosted;
                    }
                }
            }
        };
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
        match &request.bundle {
            Some(Bundle::Check(check)) => {
                match Bundles::under(&self.at.machine.scratch).check(check) {
                    Ok(dir) => environment.set("DIBS_BUNDLE", dir.display().to_string()),
                    Err(why) => {
                        drop(state);
                        say(&format!("dibs: {why}, so the command did not run.\n"));
                        begun.touch_log();
                        hosted.laid = Some(By::Dibs);
                        hosted.status = Exit::TargetRebuilt.status();
                        return hosted;
                    }
                }
            }
            Some(Bundle::Make(make)) => {
                for (name, value) in &make.env {
                    environment.give(name.clone(), value.clone());
                }
                let bundles = Bundles::under(&self.at.machine.scratch);
                let report = bundles.report(&arrived.job);
                if let Some(dir) = report.parent() {
                    let _ = fs::create_dir_all(dir);
                }
                environment.give(BundleMake::REPORT.into(), report.display().to_string());
            }
            None => {}
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
                job: &arrived.job,
                environment: &environment,
                job_dir: begun.log.as_ref().map(|_| begun.dir.as_path()),
                record: self.at.dir.file("with", pid),
                sink: self.at.sink,
            });
            state.services = services.pids();
            state.set_stage(Stage::Starting);
            drop(state);
            let cancelled = || {
                self.at
                    .call
                    .batch_id()
                    .is_some_and(|b| self.at.dir.cancelled(b))
            };
            let readied = services.ready(
                request.ready_within,
                deadline,
                &Readiness {
                    ports: &hosted.ports,
                    environment: &environment,
                    sink: self.at.sink,
                    cancelled: &cancelled,
                },
            );
            if readied != Readied::Ready {
                self.at.dir.set_deadline(pid, None);
                services.stop();
            }
            hosted.failed = matches!(readied, Readied::Failed | Readied::Cancelled);
            hosted.overran = readied == Readied::Capped;
            hosted.services = Some(services);
            state = self.at.stopper.state();
        }
        let mut unlocked = false;
        let status = match (hosted.failed, hosted.overran) {
            (true, _) => {
                drop(state);
                begun.touch_log();
                Exit::ServiceFailed.status()
            }
            (false, true) => {
                drop(state);
                begun.touch_log();
                Exit::Overran.status()
            }
            (false, false) => {
                let command = arrived.held.as_ref().map(HoldFifo::command);
                let command = command.as_deref().unwrap_or(&self.at.call.work);
                let parkable = self.parkable();
                let mut parked = false;
                let mut runs = 0;
                let mut waited = false;
                loop {
                    // Servers already started are never left running beside the urgent job, which
                    // waits for this one instead.
                    let left = deadline.left();
                    if let Some(rw) = &idle_rw {
                        // Its build's mark let go of while it is parked, so a build into the
                        // target meanwhile clears what this one left half made.
                        let marked =
                            parked && running.as_mut().is_some_and(|r| r.mark.take().is_some());
                        let until = (runs == 0).then(|| self.wait_until(arrived)).flatten();
                        let since = Instant::now();
                        let (held, yielded) =
                            self.yield_while_busy(state, rw, arrived, until, parked);
                        hosted.idled += since.elapsed();
                        parked = false;
                        state = held;
                        if let Some(status) = yielded.status() {
                            drop(state);
                            hosted.laid = Some(By::Dibs);
                            unlocked = true;
                            // Never run, its build left nothing half made.
                            if runs == 0
                                && let Some(mark) = running.as_mut().and_then(|r| r.mark.take())
                            {
                                mark.ended(JobEnd {
                                    status: 0,
                                    capped: false,
                                });
                            }
                            break status;
                        }
                        if let Some(running) = running.as_mut() {
                            // Before its first run, its mark is taken again after what was built
                            // meanwhile, which a stop of this build must not take for its own.
                            if runs == 0
                                && yielded == Yielded::Waited
                                && let Some(mark) = running.mark.take()
                            {
                                mark.ended(JobEnd {
                                    status: 0,
                                    capped: false,
                                });
                                running.mark = stepping.as_ref().and_then(Stepping::building);
                            }
                            if marked {
                                running.mark = stepping.as_ref().and_then(Stepping::building);
                            }
                        }
                        // Another tree may have built into the target while it was parked.
                        if yielded == Yielded::Waited
                            && let (Some(step), Some(stepping)) = (step, &stepping)
                            && step.claim
                        {
                            stepping.claim();
                        }
                        waited |= yielded == Yielded::Waited;
                    }
                    if hosted.services.is_none() && idle_rw.is_none() {
                        let until = (runs == 0).then(|| self.wait_until(arrived)).flatten();
                        let (held, yielded) = self.yield_to_urgent(state, until);
                        state = held;
                        if let Some(status) = yielded.status() {
                            drop(state);
                            hosted.laid = Some(By::Dibs);
                            break status;
                        }
                        waited |= yielded == Yielded::Waited;
                    }
                    if runs > 0 && self.cancelled() {
                        drop(state);
                        hosted.laid = Some(By::Dibs);
                        break Exit::Cancelled.status();
                    }
                    // Parked while a benchmark had it frozen, it waits to be let go; parked first,
                    // since that benchmark may itself wait parked behind the urgent job.
                    if frozen.now() {
                        drop(state);
                        while frozen.now() {
                            std::thread::sleep(park::POLL);
                        }
                        state = self.at.stopper.state();
                        continue;
                    }
                    // The time parked is not the command's: a rerun has its whole cap again, and a
                    // first run what it had left when it parked.
                    if runs > 0 {
                        deadline = Deadline::after((max > 0).then(|| Duration::from_secs(max)));
                        frozen.restart();
                        self.show_deadline(deadline);
                    } else if waited {
                        deadline = Deadline::after(left);
                        self.show_deadline(deadline);
                    }
                    waited = false;
                    if runs > 0 && idle_rw.is_none() {
                        // A build stopped partway leaves crates rustc would reuse half made.
                        if let Some(running) = running.as_mut()
                            && running.mark.take().is_some()
                        {
                            running.mark = stepping.as_ref().and_then(Stepping::building);
                        }
                    }
                    runs += 1;
                    let mark = running.as_ref().and_then(|r| r.mark.as_ref());
                    let work = match Job::spawn(
                        command,
                        &environment,
                        output,
                        self.at.sink,
                        mark,
                        sandbox.as_ref(),
                    ) {
                        Ok(work) => work,
                        Err(e) => {
                            drop(state);
                            self.at
                                .sink
                                .say(&format!("dibs: bash could not start: {e}\n"));
                            break NOT_STARTED;
                        }
                    };
                    state.set_stage(Stage::Running(work.pid));
                    drop(state);
                    if self.at.call.mode() == Mode::Rsh {
                        Channel::watch_transfer(
                            work.pid,
                            request.watch.lease,
                            Arc::clone(self.at.stopper),
                        );
                    }
                    let cap = deadline.left().map(|after| Cap {
                        after,
                        grace: JOB_GRACE,
                        frozen: Some(Arc::clone(&frozen)),
                    });
                    let parking = parkable.then(|| {
                        Parking::start(
                            self.at.dir.clone(),
                            pid,
                            idle_rw.is_some(),
                            Arc::clone(self.at.stopper),
                            Arc::clone(&frozen),
                        )
                    });
                    let status = self.work(work, cap, arrived.held.is_some(), &mut hosted);
                    parked = parking.is_some_and(Parking::end);
                    if !parked {
                        break status;
                    }
                    self.sweep_escaped(&arrived.job);
                    // What the caller is told of and counted is the run that finishes.
                    if let Some(log) = &begun.log {
                        let _ = fs::write(log, "");
                    }
                    hosted.overran = false;
                    state = self.at.stopper.state();
                }
            }
        };
        self.at.dir.set_deadline(pid, None);
        if let Some(services) = &mut hosted.services {
            services.stop();
        }
        if begun.log.is_some() {
            self.sweep_escaped(&arrived.job);
        }
        // Parked without the lock, it ends doing nothing beside whoever has the machine now.
        if unlocked {
            hosted.status = status;
            return hosted;
        }
        let copier = Copier::new(self.at.settings.reflinks);
        for copy in environment.copied() {
            if let Err(e) = KeptCopy::of(Path::new(copy)).give_back(copier) {
                self.at.sink.say(&format!(
                    "dibs: what the job saved in {copy} could not all go back to the machine's directory: {e}\n"
                ));
            }
        }
        hosted.status = status;
        if let (Some(step), Some(stepping)) = (step, &stepping)
            && let Some(running) = running
            && layout.step_ends(step, stepping, &mut hosted.status, hosted.overran, running)
        {
            hosted.laid = Some(By::Dibs);
        }
        if hosted.status == 0
            && let Some(Bundle::Make(make)) = &request.bundle
        {
            let made = Bundles::under(&self.at.machine.scratch).make(
                make,
                &environment,
                &arrived.job,
                &self.at.machine.host,
            );
            match made {
                Ok(manifest) => say(&format!(
                    "dibs: kept {} as bundle {}, sha256 {}\n",
                    manifest.exe, make.into, manifest.digest
                )),
                Err(why) => {
                    say(&format!("dibs: no bundle was kept: {why}\n"));
                    hosted.status = Exit::Setup.status();
                    hosted.laid = Some(By::Dibs);
                }
            }
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
        let closed = self.at.stopper.state().closed;
        let status = if cancelled {
            Exit::Cancelled.status()
        } else if closed {
            Exit::Closed.status()
        } else {
            end.hosted.status
        };
        let ran = Moment::epoch_now()
            .saturating_sub(end.acquired.at)
            .saturating_sub(end.hosted.idled.as_secs());
        {
            let mut state = self.at.stopper.state();
            state.set_stage(Stage::Finishing);
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
                _ if !cancelled
                    && !closed
                    && let Some(by) = end.hosted.laid =>
                {
                    by
                }
                Some(Exit::Overran) if end.hosted.overran => By::Dibs,
                Some(Exit::Cancelled) if cancelled => By::Dibs,
                Some(Exit::Closed) if closed => By::Dibs,
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
                    seconds: ran,
                    agent: Some(self.at.call.agent.clone()),
                    fingerprint: request.fingerprint.clone(),
                },
            );
        }
        drop(end.acquired.sharing);
        self.at.dir.clear(self.at.call.pid);
        drop(end.acquired.cards);
        drop(end.acquired.lock);
        drop(end.acquired.claim);
        if let Some(ended) = &ended {
            ended.report();
        }
        if status == Exit::Overran.status() && end.hosted.overran {
            self.overran(end.max);
        }
        status
    }

    /// The image a build runs in, unpacked on this machine within the job's cap.
    fn sandbox(
        &self,
        image: &Image,
        environment: &Environment,
        deadline: Deadline,
    ) -> Result<Sandbox, Unready> {
        let home = home();
        let images = Images {
            scratch: &self.at.machine.scratch,
            home: &home,
        };
        let running = |pid| self.at.stopper.state().set_stage(Stage::Preparing(pid));
        let unstopped = |act: &mut dyn FnMut()| {
            let _state = self.at.stopper.state();
            act();
        };
        let commands = Commands::new(environment, &running, &unstopped, deadline);
        images.prepare(image, &commands)
    }

    /// What the command started outside its group, as `setsid` does, which the group's own sweep
    /// cannot reach and which would otherwise hold cards and memory past the lock.
    fn sweep_escaped(&self, job: &JobId) {
        let me = std::process::id();
        let left: Vec<u32> = Host::processes()
            .iter()
            .map(|process| process.pid)
            .filter(|&pid| pid != me)
            .filter(|&pid| Host::variable(pid, "DIBS_JOB").as_deref() == Some(job.as_str()))
            .collect();
        if left.is_empty() {
            return;
        }
        let described: String = left
            .iter()
            .map(|&pid| {
                let line = Host::describe(pid).unwrap_or_else(|| pid.to_string());
                format!("  {}\n", line.chars().take(DESCRIBED).collect::<String>())
            })
            .collect();
        Host::reap(&left);
        self.at.sink.say(&format!(
            "dibs: stopped what the command left running outside its process group:\n{described}"
        ));
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
