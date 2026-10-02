use crate::{
    call::{
        card::{CardError, unpinned},
        hold::Hold,
        machine::MachineCall,
        series::{Entry, Moved, Series, stayed_put},
    },
    caller::{Caller, short_hostname},
    cli::{Call, Command, PortName, Run, RunLock, Service},
    machine::{
        CallValues, Card, Fleet, Here, Liveness, MachineHalf, MaxFrom, Named, Session, Target,
        TargetEnv, TargetError, exit_code,
    },
    paths::Paths,
    placement::{Placement, Unplaced},
};
use dibs_format::{Exit, Label, Mode};
use std::{
    fmt,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

/// The most lines of a batch's plan a call carries.
const BATCH_PLAN_LINES: usize = 200;
const FINGERPRINT_CHARS: usize = 16;

/// A call that takes the lock, or peeks past it: what the Rust client runs itself.
pub struct LockedCall<'a> {
    mode: Mode,
    hold: bool,
    command: &'a Command,
    services: &'a [Service],
    ports: &'a [PortName],
    ready_within: u32,
}

#[derive(Debug)]
pub enum CallError {
    Target(TargetError),
    Card(CardError),
    Moved(Moved),
    /// The lock is already held around this call, so it would queue behind itself.
    InsideHold {
        lock_at: String,
    },
    Unplaced(Unplaced),
    Io(std::io::Error),
}

/// What the environment says about this call, read once.
struct Environment {
    /// `DIBS_LOCAL=1`: the lock is taken on this computer.
    local: bool,
    /// `DIBS_FROM_RUN=1`: the recipe layer is the caller.
    from_run: bool,
    series_check: bool,
    unpinned_quiet: bool,
}

impl<'a> LockedCall<'a> {
    pub fn run_of(run: &'a Run) -> LockedCall<'a> {
        LockedCall {
            mode: match run.lock {
                RunLock::Shared => Mode::Shared,
                RunLock::Bench => Mode::Bench,
            },
            hold: run.hold,
            command: &run.command,
            services: &run.services,
            ports: &run.ports,
            ready_within: run.ready_within,
        }
    }

    pub fn peek(command: &'a Command) -> LockedCall<'a> {
        LockedCall {
            mode: Mode::Peek,
            ..LockedCall::shared(command)
        }
    }

    /// A shared run of a command the client wrote itself.
    pub fn shared(command: &'a Command) -> LockedCall<'a> {
        LockedCall {
            mode: Mode::Shared,
            hold: false,
            command,
            services: &[],
            ports: &[],
            ready_within: Run::default().ready_within,
        }
    }

    fn bench(&self) -> bool {
        self.mode == Mode::Bench
    }

    /// Runs the call and returns the exit to give.
    pub fn run(&self, call: &Call, caller: &Caller) -> Result<i32, CallError> {
        let env = Environment::read();
        let paths = Paths::from_env();
        let fleet = Fleet::load(paths.inventory());
        let here = Here {
            name: short_hostname(None),
            local: env.local,
        };
        let target = self.target(call, &env, &fleet)?;

        let card = match &call.device {
            Some(alias) => Card::resolve(alias, &target, &fleet)?,
            None => Card::none(),
        };
        if self.bench()
            && call.device.is_none()
            && !self.hold
            && !call.preflight
            && !env.unpinned_quiet
            && let Some(note) = target.entry(&fleet).and_then(unpinned)
        {
            eprint!("{note}");
        }

        let label = Request::label(call);
        let device = call
            .device
            .as_ref()
            .map_or("none".to_string(), |d| d.to_string());
        let machine = series_machine(&target);
        let entry = Entry {
            label: label.as_str(),
            machine: &machine,
            card: &device,
        };
        let series = paths.series().map(|path| Series { path });
        if self.bench()
            && !self.hold
            && env.series_check
            && !call.new_series
            && let Some(series) = &series
            && let Some(note) = series.check(&entry, &fleet, env.from_run && !call.preflight)?
        {
            eprint!("{note}");
        }
        if call.preflight {
            return Ok(0);
        }

        let session = Session::new(&target, &here);
        if matches!(self.mode, Mode::Shared | Mode::Bench) && holding(&session.lock_at) {
            return Err(CallError::InsideHold {
                lock_at: session.lock_at,
            });
        }
        let values = self.values(call, caller, label.clone(), card);
        let half = MachineHalf::load()?;
        let live = Liveness::from_env();
        let status = match self.hold {
            true => Hold {
                command: self.command,
                lock: self.mode.as_str(),
                at: session.at(&target, &here),
                lock_at: session.lock_at.clone(),
                reach: session.reach(&target, &here),
            }
            .run(session.hold(&values, &half, live)?)?,
            false => exit_code(session.run(&values, &half, live)?),
        };

        if self.bench()
            && !self.hold
            && status == 0
            && env.series_check
            && let Some(series) = &series
        {
            series.record(&entry, &caller.name, call.new_series);
        }
        if self.bench() && call.new_series && status != 0 {
            eprint!("{}", stayed_put(label.as_str()));
        }
        Ok(session.exit(status, &target))
    }

    /// The machine the call goes to, placed when it is shared work that names none.
    fn target(&self, call: &Call, env: &Environment, fleet: &Fleet) -> Result<Target, CallError> {
        let mut target = Target::resolve(call.on.as_ref(), &TargetEnv::from_env(), fleet)?;
        if self.bench()
            && !target.measurable
            && !self.hold
            && let Some(machine) = &target.machine
        {
            return Err(TargetError::NotMeasured(machine.clone()).into());
        }
        let placed = self.mode == Mode::Shared
            && !target.pinned()
            && target.host.is_empty()
            && fleet.names().len() > 1
            && !env.local
            && !self.hold
            && !env.from_run;
        if placed {
            let caller = Caller::default();
            let machine = Placement {
                machine: &MachineCall::new(call, &caller),
            }
            .pick()?;
            target.go_to(fleet, machine.as_str(), Named::Placed)?;
        }
        if target.host.is_empty() && !env.local {
            return Err(target.no_machine(fleet, self.bench()).into());
        }
        Ok(target)
    }

    fn values(&self, call: &Call, caller: &Caller, label: Label, card: Card) -> CallValues {
        CallValues {
            ready_within: self.ready_within,
            ports: self.ports.to_vec(),
            services: self.services.to_vec(),
            ..Request {
                mode: self.mode,
                call,
                caller,
            }
            .values(label, card, self.command.shell_string())
        }
    }
}

/// A call's values before its mode adds its own, as every mode sends them.
pub(crate) struct Request<'a> {
    pub mode: Mode,
    pub call: &'a Call,
    pub caller: &'a Caller,
}

impl Request<'_> {
    pub fn values(&self, label: Label, card: Card, command: String) -> CallValues {
        let (max, max_from) = match self.call.max {
            Some(max) => (max, MaxFrom::Given),
            None => (default_max(self.mode), MaxFrom::Default),
        };
        let stream = match self.call.stream {
            true => "1".to_string(),
            false => set("DIBS_STREAM")
                .or_else(|| set("DIBS_FROM_RUN"))
                .unwrap_or_else(|| "0".into()),
        };
        let fingerprint: String = std::env::var("DIBS_FINGERPRINT")
            .unwrap_or_default()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
            .take(FINGERPRINT_CHARS)
            .collect();
        let caller = match self.mode {
            Mode::Shared
            | Mode::Bench
            | Mode::Peek
            | Mode::Rsh
            | Mode::Kill
            | Mode::KillForce
            | Mode::Gc => self.caller.clone(),
            _ => Caller::default(),
        };
        let batch = match self.mode {
            Mode::Shared | Mode::Bench | Mode::Peek | Mode::Rsh => batch_plan(),
            _ => String::new(),
        };
        CallValues {
            mode: self.mode,
            label,
            wait: self.call.wait,
            max,
            max_from,
            verbose: self.call.verbose,
            json: self.call.json,
            card,
            stream,
            ready_within: crate::cli::Run::default().ready_within,
            fingerprint,
            command,
            // SAFETY: isatty only reads the descriptor's state.
            tty: unsafe { libc::isatty(1) } == 1,
            caller,
            batch,
            ports: Vec::new(),
            services: Vec::new(),
        }
    }

    /// `--label`, or the directory the call was made from, as the machine files it.
    pub fn label(call: &Call) -> Label {
        call.label
            .clone()
            .unwrap_or_else(|| Label::new(directory_name()))
            .filed()
    }
}

/// How long a call may hold the lock when `--max` does not say.
fn default_max(mode: Mode) -> u64 {
    match mode {
        Mode::Bench => 7200,
        Mode::Peek => 30,
        Mode::Rsh => 3600,
        _ => 1800,
    }
}

impl Environment {
    fn read() -> Environment {
        let is = |k: &str, v: &str| std::env::var(k).is_ok_and(|x| x == v);
        Environment {
            local: is("DIBS_LOCAL", "1"),
            from_run: is("DIBS_FROM_RUN", "1"),
            series_check: set("DIBS_SERIES_CHECK").is_none_or(|v| v == "1"),
            unpinned_quiet: is("DIBS_UNPINNED_QUIET", "1"),
        }
    }
}

/// A variable that is set and not empty, as a shell's `${VAR:-}` tests it.
fn set(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Inside a `--hold` of this machine's lock, a call that takes it again queues behind the hold,
/// which only ends when the call does.
pub(crate) fn holding(lock_at: &str) -> bool {
    let held = std::env::var("DIBS_HOLDING").unwrap_or_default();
    format!(" {held} ").contains(&format!(" {lock_at} "))
}

/// Keyed on where the job goes, so one machine reached by two names is one series.
fn series_machine(target: &Target) -> String {
    [
        target.host.clone(),
        target
            .machine
            .as_ref()
            .map(|m| m.to_string())
            .unwrap_or_default(),
        target.hostname.clone(),
    ]
    .into_iter()
    .find(|m| !m.is_empty())
    .unwrap_or_else(|| "?".into())
}

/// Which batch this call is a step of, and what is still to come, so the machine can say how
/// long the batch has left.
fn batch_plan() -> String {
    let Some(batch) = set("DIBS_BATCH") else {
        return String::new();
    };
    let step = std::env::var("DIBS_BATCH_STEP").unwrap_or_default();
    let plan = std::env::var("DIBS_BATCH_PLAN").unwrap_or_default();
    let text = format!("{batch}\t{step}\n{plan}\n");
    let kept: String = text.split_inclusive('\n').take(BATCH_PLAN_LINES).collect();
    kept.trim_end_matches('\n').to_string()
}

/// The name of the directory the call was made from, as the shell sees it.
fn directory_name() -> String {
    let physical = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let same = |a: &Path, b: &Path| match (a.metadata(), b.metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    };
    let logical = std::env::var_os("PWD")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && same(p, &physical))
        .unwrap_or(physical);
    logical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into())
}

impl CallError {
    pub fn exit(&self) -> i32 {
        match self {
            CallError::Unplaced(e) => i32::from(e.exit().code()),
            CallError::Io(_) => i32::from(Exit::Failed.code()),
            _ => i32::from(Exit::Refused.code()),
        }
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Target(e) => e.fmt(f),
            CallError::Card(e) => e.fmt(f),
            CallError::Moved(e) => e.fmt(f),
            CallError::InsideHold { lock_at } => {
                writeln!(
                    f,
                    "dibs: this runs inside a --hold of the lock on {lock_at}, so it would queue behind that hold and never start."
                )?;
                writeln!(
                    f,
                    "  Take the lock once: run this outside the --hold, or peek if it is free to run."
                )
            }
            CallError::Unplaced(e) => e.fmt(f),
            CallError::Io(e) => writeln!(f, "dibs: {e}"),
        }
    }
}

impl From<TargetError> for CallError {
    fn from(e: TargetError) -> CallError {
        CallError::Target(e)
    }
}

impl From<CardError> for CallError {
    fn from(e: CardError) -> CallError {
        CallError::Card(e)
    }
}

impl From<Moved> for CallError {
    fn from(e: Moved) -> CallError {
        CallError::Moved(e)
    }
}

impl From<Unplaced> for CallError {
    fn from(e: Unplaced) -> CallError {
        CallError::Unplaced(e)
    }
}

impl From<std::io::Error> for CallError {
    fn from(e: std::io::Error) -> CallError {
        CallError::Io(e)
    }
}
