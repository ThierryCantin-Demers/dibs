//! The jobs a recipe runs: locked calls made in this process, whose output is read here for the
//! trailer and for the tree the machine lays out ahead of its command.

use crate::{
    call::{CallError, Destination, LockedCall, MachineCall, Origin, Output, RecipeJob, Sync},
    caller::Caller,
    cli::{Call, Command as ShellCommand, Mode, Run, RunLock},
    execution::RunError,
    machine::{Interrupt, Listener, Stream},
    placement::Placement,
    recipe::Lock,
};
use dibs_format::{
    Alias, JobId, Label, MachineName, StepRecord,
    wire::{Prepared, Stepped, Tree},
};
use std::{io::Write as _, time::Instant};

/// What a run record names the layer its jobs ran on.
pub const BACKEND: &str = "dibs";

pub struct JobRequest<'a> {
    pub label: &'a str,
    pub lock: Lock,
    /// The card to run on, named from the machine's inventory. Absent means the runtime
    /// picks, which is fine for a build and is what makes two benchmarks incomparable.
    pub device: Option<&'a str>,
    /// Which batch this is a step of and what is still to come, for the machine's status.
    pub job: &'a RecipeJob,
    /// Seconds the job may hold the lock, when the caller knows the default is too short.
    pub max: Option<u64>,
    /// A measurement that starts its label's series on this machine again, on another card.
    pub new_series: bool,
    /// The tree it runs in, laid out at its head when it is not yet.
    pub tree: Option<Tree>,
}

pub struct JobOutcome {
    pub status: i32,
    pub seconds: u64,
    /// What the job's trailer said, when its stderr passed through here and it printed one.
    pub trailer: Option<Trailer>,
}

/// A job's outcome, the tree the machine laid out for it, and what it did around a step.
pub struct Reported {
    pub outcome: JobOutcome,
    pub prepared: Option<Prepared>,
    pub stepped: Option<Stepped>,
}

impl JobRequest<'_> {
    /// What the machine is told of the job besides its command.
    fn recipe_job(&self) -> RecipeJob {
        RecipeJob {
            tree: self.tree.clone(),
            ..self.job.clone()
        }
    }
}

impl JobOutcome {
    /// The record of the step this was the outcome of.
    pub fn step_record(&self, lock: Lock) -> StepRecord {
        let trailer = self.trailer.as_ref();
        StepRecord {
            lock,
            status: self.status,
            seconds: self.seconds,
            arm: None,
            rep: None,
            artifacts: None,
            job: trailer.map(|t| JobId::new(t.job.as_str())),
            built: trailer.and_then(|t| t.built.clone()),
            log: trailer.and_then(|t| t.log.clone()),
        }
    }
}

pub struct Trailer {
    pub job: String,
    /// `built=` as printed: a count of crates, or `nothing`. Absent when cargo did not run.
    pub built: Option<String>,
    /// `<host>:<path>` of the job's whole log.
    pub log: Option<String>,
}

impl Trailer {
    /// Reads `job <id>  <mode>  <label>  ...  built=N` and the `  log <host>:<path>` after it. The
    /// last job wins: a call is one job, and anything before it was a setup riding ahead.
    fn read(line: &str, into: &mut Option<Trailer>) {
        if let Some(rest) = line.strip_prefix("job ") {
            let mut words = rest.split_whitespace();
            if let Some(job) = words.next() {
                let built = words
                    .find_map(|w| w.strip_prefix("built="))
                    .map(str::to_string);
                *into = Some(Trailer {
                    job: job.to_string(),
                    built,
                    log: None,
                });
            }
        } else if let (Some(rest), Some(t)) = (line.strip_prefix("  log "), into.as_mut()) {
            t.log = rest.split_whitespace().next().map(str::to_string);
        }
    }
}

/// A job's output as it arrives: passed through, with what the recipe layer reads kept aside.
struct Reader<'a> {
    on_prepared: &'a mut dyn FnMut(&Prepared),
    prepared: Option<Prepared>,
    stepped: Option<Stepped>,
    trailer: Option<Trailer>,
    start: Instant,
}

impl<'a> Reader<'a> {
    fn new(on_prepared: &'a mut dyn FnMut(&Prepared)) -> Reader<'a> {
        Reader {
            on_prepared,
            prepared: None,
            stepped: None,
            trailer: None,
            start: Instant::now(),
        }
    }

    fn pass(stream: Stream, line: &[u8]) {
        let _ = match stream {
            Stream::Err => {
                let mut e = std::io::stderr().lock();
                e.write_all(line).and_then(|_| e.flush())
            }
            Stream::Out => {
                let mut o = std::io::stdout().lock();
                o.write_all(line).and_then(|_| o.flush())
            }
        };
    }

    fn reported(self, status: i32) -> Reported {
        Reported {
            outcome: JobOutcome {
                status,
                seconds: self.start.elapsed().as_secs(),
                trailer: self.trailer,
            },
            prepared: self.prepared,
            stepped: self.stepped,
        }
    }
}

impl Listener for Reader<'_> {
    /// Passed through, the trailer read on the way.
    fn line(&mut self, stream: Stream, line: &[u8]) {
        if stream == Stream::Err {
            Trailer::read(String::from_utf8_lossy(line).trim_end(), &mut self.trailer);
        }
        Reader::pass(stream, line);
    }

    fn prepared(&mut self, prepared: &Prepared) {
        (self.on_prepared)(prepared);
        self.prepared = Some(prepared.clone());
    }

    fn stepped(&mut self, stepped: &Stepped) {
        self.stepped = Some(stepped.clone());
    }
}

/// Where a recipe's jobs go: chosen once per run and held for every step. Picking per step would
/// put the build on one machine and the command that needs its worktree on another.
pub struct Jobs {
    pub machine: Option<MachineName>,
    caller: Caller,
}

impl Jobs {
    pub fn on(machine: Option<MachineName>) -> Jobs {
        Jobs {
            machine,
            caller: Caller::from_env(),
        }
    }

    /// Where shared work goes: the machine named, or the only one there is, or else a placement
    /// among them by build cache and then by load. Only for work that is shared throughout: a
    /// measurement's history keys on the machine it ran on.
    ///
    /// `prefer` names the machine already holding this repo's build cache; `repo` asks which
    /// machines hold it, which is what makes the first run for a repo land somewhere useful.
    pub fn placed(
        on: Option<MachineName>,
        prefer: Option<&str>,
        repo: Option<&str>,
    ) -> Result<Jobs, RunError> {
        let call = Call {
            mode: Mode::Pick,
            on,
            prefer: prefer.map(str::to_string),
            repo: repo.map(str::to_string),
            ..Call::default()
        };
        let caller = Caller::default();
        let machine = MachineCall::new(&call, &caller)?;
        let placed = match machine.destination()? {
            Destination::Named(m) => Some(m),
            Destination::Unnamed => None,
            Destination::Unchosen => Some(Placement { machine: &machine }.pick().map_err(|_| {
                let why = repo.map(|r| format!(" --repo {r}")).unwrap_or_default();
                format!(
                    "nowhere to send this: it names no machine, and none could be placed. dibs --pick -v{why}\n  \
                     says why; --on <machine> names one."
                )
            })?),
        };
        Ok(Jobs::on(placed))
    }

    /// The machine a call goes to with no ranking at all. Naming it matters even when there was
    /// no choice to make: a benchmark cannot be moved, so it is the one that decides where its
    /// repo's build cache belongs, and the record should say where it ran.
    pub fn destination(on: Option<MachineName>) -> Result<Destination, RunError> {
        let call = Call {
            on,
            ..Call::default()
        };
        let caller = Caller::default();
        Ok(MachineCall::new(&call, &caller)?.destination()?)
    }

    pub fn run(&self, req: &JobRequest, command: &str) -> JobOutcome {
        self.read(req, command, Reader::new(&mut |_| {})).outcome
    }

    /// Output streams as `run`'s does, and the tree the machine lays out at the job's head is
    /// handed to `on_prepared` before anything after it is shown.
    pub fn run_reporting(
        &self,
        req: &JobRequest,
        command: &str,
        on_prepared: &mut dyn FnMut(&Prepared),
    ) -> Reported {
        self.read(req, command, Reader::new(on_prepared))
    }

    /// Whether the call would be refused on grounds decided here, without the machine: a machine
    /// that does not measure, or a label measured somewhere else.
    pub fn preflight(&self, req: &JobRequest) -> bool {
        let call = Call {
            preflight: true,
            ..self.call(req, "true")
        };
        Jobs::exit(self.locked(&call, req.job, &mut Output::Inherit)) == 0
    }

    /// rsync between here and the machine, with the request's tree laid out there first under
    /// the same shared lock, and handed to `on_prepared` as `run_reporting` hands one.
    pub fn sync(
        &self,
        req: &JobRequest,
        args: &[String],
        on_prepared: &mut dyn FnMut(&Prepared),
    ) -> Reported {
        let mut reader = Reader::new(on_prepared);
        let call = self.call(req, "");
        let job = req.recipe_job();
        let exit = MachineCall::new(&call, &self.caller).and_then(|machine| {
            Sync {
                machine: &machine,
                args,
                origin: Origin::Recipe(&job),
            }
            .answer_into(&mut Output::Listening(&mut reader))
        });
        reader.reported(Jobs::exit(exit))
    }

    /// What a job kept, fetched into `into`: the report of it, or the exit that stopped it.
    pub fn fetch(&self, job: &JobId, into: Option<&str>) -> Result<String, i32> {
        let call = Call {
            on: self.machine.clone(),
            ..Call::default()
        };
        let mut report = Vec::new();
        let exit = MachineCall::new(&call, &self.caller)
            .and_then(|machine| machine.fetch(job, into, &mut report));
        match Jobs::exit(exit) {
            0 => Ok(String::from_utf8_lossy(&report).into_owned()),
            exit => Err(exit),
        }
    }

    fn read(&self, req: &JobRequest, command: &str, mut reader: Reader) -> Reported {
        let call = self.call(req, command);
        let exit = self.locked(
            &call,
            &req.recipe_job(),
            &mut Output::Listening(&mut reader),
        );
        reader.reported(Jobs::exit(exit))
    }

    fn locked(&self, call: &Call, job: &RecipeJob, output: &mut Output) -> Result<i32, CallError> {
        let Mode::Run(run) = &call.mode else {
            unreachable!("a recipe's call is a run");
        };
        LockedCall::run_of(run)
            .made_by(Origin::Recipe(job))
            .run_into(call, &self.caller, output)
    }

    /// A call's exit, with what refused it said. A Ctrl-C the job was given ends the run here too,
    /// once the job has released its lock.
    fn exit(exit: Result<i32, CallError>) -> i32 {
        let exit = exit.unwrap_or_else(|e| {
            eprint!("{e}");
            e.exit()
        });
        if Interrupt::heard() {
            Interrupt::raise();
        }
        exit
    }

    fn call(&self, req: &JobRequest, command: &str) -> Call {
        let lock = match req.lock {
            Lock::Exclusive => RunLock::Bench,
            Lock::Shared => RunLock::Shared,
        };
        Call {
            mode: Mode::Run(Run {
                lock,
                command: ShellCommand(vec![command.to_string()]),
                ..Run::default()
            }),
            on: self.machine.clone(),
            label: (!req.label.is_empty()).then(|| Label::new(req.label)),
            max: req.max,
            device: req.device.map(Alias::new),
            new_series: req.new_series && req.lock == Lock::Exclusive,
            stream: true,
            ..Call::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trailer_is_read_from_the_last_job_a_call_printed() {
        let err = "job 1-1  shared  a:setup  queued 0s  ran 1s  exit 0  by=command\n  log m:/j/1-1/log  (3 lines)  dibs --out 1-1\n\
                   job 1-2  shared  a  queued 0s  ran 9s  exit 0  by=command  built=nothing\n  log m:/j/1-2/log  (40 lines)  dibs --out 1-2\n";
        let mut t = None;
        for l in err.lines() {
            Trailer::read(l, &mut t);
        }
        let t = t.unwrap();
        assert_eq!(
            (t.job.as_str(), t.built.as_deref(), t.log.as_deref()),
            ("1-2", Some("nothing"), Some("m:/j/1-2/log"))
        );
    }

    #[test]
    fn a_new_series_reaches_the_measurement_and_nothing_else() {
        let job = RecipeJob::default();
        let jobs = Jobs::on(Some(MachineName::new("m")));
        let new_series = |lock, new_series| {
            let req = JobRequest {
                label: "a/bench/x",
                lock,
                device: None,
                job: &job,
                max: None,
                new_series,
                tree: None,
            };
            jobs.call(&req, "cmd").new_series
        };
        assert!(new_series(Lock::Exclusive, true));
        assert!(!new_series(Lock::Shared, true));
        assert!(!new_series(Lock::Exclusive, false));
    }
}
