//! The jobs a recipe runs: locked calls made in this process, whose output is read here for the
//! trailer and for what a setup reports ahead of its command.

use crate::recipe::Lock;
use dibs::{
    call::{CallError, Destination, LockedCall, MachineCall, Origin, Output, RecipeJob, Sync},
    caller::Caller,
    cli::{Call, Command, Mode, Run, RunLock},
    machine::{Interrupt, Stream},
    placement::Placement,
};
use dibs_format::{Alias, JobId, Label, MachineName, StepRecord};
use std::{io::Write as _, time::Instant};

/// What a run record names the layer its jobs ran on.
pub const BACKEND: &str = "dibs";

pub struct Request<'a> {
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
}

pub struct Outcome {
    pub status: i32,
    pub seconds: u64,
    /// What the job's trailer said, when its stderr passed through here and it printed one.
    pub trailer: Option<Trailer>,
}

/// A job's outcome, and the `DIBS-` lines it reported, or its whole stdout when that was kept.
pub struct Reported {
    pub outcome: Outcome,
    pub text: String,
}

impl Outcome {
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

/// What a job's output is read for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// Passed through, apart from the `DIBS-` lines a setup reports and the trailer.
    Reported,
    /// Stdout kept whole, stderr passed through.
    Kept,
}

/// A job's output as it arrives: passed through, with what the recipe layer reads kept aside.
struct Reader<'a> {
    reading: Reading,
    on_report: &'a mut dyn FnMut(&str),
    text: String,
    told: bool,
    trailer: Option<Trailer>,
    start: Instant,
}

impl<'a> Reader<'a> {
    fn new(reading: Reading, on_report: &'a mut dyn FnMut(&str)) -> Reader<'a> {
        Reader {
            reading,
            on_report,
            text: String::new(),
            told: false,
            trailer: None,
            start: Instant::now(),
        }
    }

    fn line(&mut self, stream: Stream, line: &[u8]) {
        if self.reading == Reading::Kept {
            match stream {
                Stream::Out => self.text.push_str(&String::from_utf8_lossy(line)),
                Stream::Err => Reader::pass(stream, line),
            }
            return;
        }
        if stream == Stream::Err {
            Trailer::read(String::from_utf8_lossy(line).trim_end(), &mut self.trailer);
        }
        if !line.starts_with(b"DIBS-") {
            return Reader::pass(stream, line);
        }
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end();
        self.text.push_str(text);
        self.text.push('\n');
        if !self.told && (text == "DIBS-READY" || text == "DIBS-HELD") {
            self.told = true;
            (self.on_report)(&self.text);
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
        let trailer = match self.reading {
            Reading::Reported => self.trailer,
            Reading::Kept => None,
        };
        Reported {
            outcome: Outcome {
                status,
                seconds: self.start.elapsed().as_secs(),
                trailer,
            },
            text: self.text,
        }
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
    pub fn placed(prefer: Option<&str>, repo: Option<&str>) -> Result<Jobs, String> {
        let call = Call {
            mode: Mode::Pick,
            prefer: prefer.map(str::to_string),
            repo: repo.map(str::to_string),
            ..Call::default()
        };
        let caller = Caller::default();
        let machine = MachineCall::new(&call, &caller);
        let placed = match machine.destination() {
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
    pub fn destination() -> Destination {
        let call = Call::default();
        let caller = Caller::default();
        MachineCall::new(&call, &caller).destination()
    }

    pub fn run(&self, req: &Request, command: &str) -> Outcome {
        self.read(req, command, Reader::new(Reading::Reported, &mut |_| {}))
            .outcome
    }

    /// Same, but the job's stdout comes back rather than going to the terminal. For setup steps
    /// that have to report where they put things; a benchmark's output must keep streaming to
    /// whoever asked for it.
    pub fn run_capture(&self, req: &Request, command: &str) -> Reported {
        self.read(req, command, Reader::new(Reading::Kept, &mut |_| {}))
    }

    /// Output streams as `run`'s does, except the report of a setup run ahead of the command,
    /// which is collected and handed to `on_report` before anything after it is shown.
    pub fn run_reporting(
        &self,
        req: &Request,
        command: &str,
        on_report: &mut dyn FnMut(&str),
    ) -> Reported {
        self.read(req, command, Reader::new(Reading::Reported, on_report))
    }

    /// Whether the call would be refused on grounds decided here, without the machine: a machine
    /// that does not measure, or a label measured somewhere else.
    pub fn preflight(&self, req: &Request) -> bool {
        let call = Call {
            preflight: true,
            ..self.call(req, "true")
        };
        Jobs::exit(self.locked(&call, req.job, &mut Output::Inherit)) == 0
    }

    /// rsync between here and the machine, with `before` run there first under the same shared
    /// lock, its report read as `run_reporting` reads one.
    pub fn sync(
        &self,
        req: &Request,
        args: &[String],
        before: &str,
        on_report: &mut dyn FnMut(&str),
    ) -> Reported {
        let mut reader = Reader::new(Reading::Reported, on_report);
        let call = self.call(req, "");
        let machine = MachineCall::new(&call, &self.caller);
        let exit = Sync {
            machine: &machine,
            args,
            before,
            origin: Origin::Recipe(req.job),
        }
        .answer_into(&mut Output::Lines(&mut |stream, line| {
            reader.line(stream, line)
        }));
        reader.reported(Jobs::exit(exit))
    }

    /// What a job kept, fetched into `into`: the report of it, or the exit that stopped it.
    pub fn fetch(&self, job: &JobId, into: Option<&str>) -> Result<String, i32> {
        let call = Call {
            on: self.machine.clone(),
            ..Call::default()
        };
        let mut report = Vec::new();
        let exit = MachineCall::new(&call, &self.caller).fetch(job, into, &mut report);
        match Jobs::exit(exit) {
            0 => Ok(String::from_utf8_lossy(&report).into_owned()),
            exit => Err(exit),
        }
    }

    fn read(&self, req: &Request, command: &str, mut reader: Reader) -> Reported {
        let call = self.call(req, command);
        let exit = self.locked(
            &call,
            req.job,
            &mut Output::Lines(&mut |stream, line| reader.line(stream, line)),
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

    fn call(&self, req: &Request, command: &str) -> Call {
        let lock = match req.lock {
            Lock::Exclusive => RunLock::Bench,
            Lock::Shared => RunLock::Shared,
        };
        Call {
            mode: Mode::Run(Run {
                lock,
                command: Command(vec![command.to_string()]),
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
            let req = Request {
                label: "a/bench/x",
                lock,
                device: None,
                job: &job,
                max: None,
                new_series,
            };
            jobs.call(&req, "cmd").new_series
        };
        assert!(new_series(Lock::Exclusive, true));
        assert!(!new_series(Lock::Shared, true));
        assert!(!new_series(Lock::Exclusive, false));
    }
}
