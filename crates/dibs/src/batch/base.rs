use super::{
    check_in::Hint,
    guard::{Carried, StepGuard},
    parse::{BatchError, Step, StepKind, parse},
    plan::Batch,
};
use crate::{
    call::{
        BatchStep, CHECK_IN, Destination, Driver, IMAGE, LOW_PRIORITY, MachineCall, Pending,
        reached_its_job,
    },
    caller::Caller,
    cli::Call,
    execution::Job,
    itself::Itself,
    paths::{FileError, Paths},
};
use dibs_format::{BatchId, Exit, MachineName, Span};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum State {
    Waiting,
    Running,
    Done {
        exit: i32,
        seconds: u64,
    },
    NotRun,
    /// Its job runs on without the step's call, which the hint's follow waits for.
    CheckedIn {
        hint: Hint,
        seconds: u64,
    },
}

/// A batch paused once a step checked in, as its driver left it for `dibs batch --follow`.
#[derive(Debug, Serialize, Deserialize)]
struct Paused {
    text: String,
    /// Where the driver ran, which its steps' own trees and labels are read from.
    cwd: PathBuf,
    on: Option<String>,
    /// `DIBS_CHECK_IN` as the batch was started with, for the steps a follow starts.
    #[serde(default)]
    check_in: Option<String>,
    /// `--low-priority`, which the steps a follow starts keep.
    #[serde(default)]
    low_priority: bool,
    /// `DIBS_IMAGE` as the batch was started with, for the steps a follow starts.
    #[serde(default)]
    image: Option<String>,
    verbose: bool,
    states: Vec<State>,
    /// How long the batch had run, its pauses not counted.
    seconds: u64,
}

impl Paused {
    const FILE: &'static str = "paused";
}

/// Whether the batch in `dir` is driven here, so `resume` follows it: paused by a step that
/// checked in, or ended.
pub fn driven_here(dir: &Path) -> bool {
    dir.join(Paused::FILE).is_file()
        || (dir.join(EXIT).is_file() && !dir.join(Driver::DETACHED).exists())
}

/// What a step printed on stderr, read for what dibs said of its jobs.
#[derive(Clone, Copy)]
pub struct StepStderr<'a>(pub &'a str);

impl<'a> StepStderr<'a> {
    /// The jobs a step's trailers named, in order, each with what its trailer says about the exit
    /// and the build: `by=dibs` when dibs produced the exit, and `built=`, which is what says
    /// whether a measurement ran on a fresh binary. A recipe step prints several.
    pub fn jobs(&self) -> Vec<String> {
        self.0
            .lines()
            .filter_map(|l| l.strip_prefix("job "))
            .filter_map(|r| {
                let mut words = r.split_whitespace();
                let id = words.next()?;
                let verdict: Vec<&str> = words
                    .filter(|w| *w == "by=dibs" || *w == "tests=0" || w.starts_with("built="))
                    .collect();
                Some(if verdict.is_empty() {
                    id.to_string()
                } else {
                    format!("{id} {}", verdict.join(" "))
                })
            })
            .collect()
    }

    /// The locks a step's jobs took, as their trailers name them, each once in the order taken:
    /// what a measurement ran under, which a step's own words do not always say.
    pub fn locks(&self) -> Vec<&'a str> {
        let mut locks = Vec::new();
        for lock in self
            .0
            .lines()
            .filter_map(|l| l.strip_prefix("job "))
            .filter_map(|r| r.split_whitespace().nth(1))
        {
            if !locks.contains(&lock) {
                locks.push(lock);
            }
        }
        locks
    }

    /// The working trees the step's call took as `@local`, a pin's and a comparison's arm's
    /// included, as it announced them: a batch run from the wrong directory reads as wrong only
    /// here.
    pub fn local_trees(&self) -> Vec<&'a str> {
        self.0
            .lines()
            .filter_map(|l| {
                let arm = l
                    .strip_prefix("  ")
                    .and_then(|l| l.split_once(": "))
                    .filter(|(arm, _)| !arm.contains(' '))
                    .map(|(_, tree)| tree);
                l.strip_prefix("dibs: preparing ")
                    .or_else(|| l.strip_prefix("dibs: pinning "))
                    .or(arm)
            })
            .filter(|l| {
                l.contains(" from ")
                    && (l.ends_with(" (clean)") || l.ends_with(" (uncommitted changes included)"))
            })
            .collect()
    }

    /// What the step's call said as it let go of its job, the ways on from it left out: the
    /// batch is followed as a whole.
    pub fn check_in(&self) -> String {
        let lines: Vec<&str> = self.0.lines().collect();
        let Some(from) = lines
            .iter()
            .rposition(|l| l.contains("dibs: job ") && l.contains("and still runs"))
        else {
            return String::new();
        };
        lines[from..]
            .iter()
            .filter(|l| !l.trim_start().starts_with("dibs "))
            .map(|l| format!("{l}\n"))
            .collect()
    }

    /// A command may exit 76 of its own accord, so only dibs saying so makes it a cancellation.
    pub fn cancelled(&self) -> bool {
        self.0.contains("was cancelled with dibs --kill")
            || self.0.lines().any(|l| {
                l.starts_with("job ")
                    && l.contains(&format!("  exit {}  by=dibs", Exit::Cancelled.code()))
            })
    }

    /// Why a step that failed before any job ran stopped: the last thing dibs said, a refusal
    /// or the prepare that could not be done, after the last explanation it gave, which is
    /// where the cause is when that last line only restates the exit.
    pub fn why_it_stopped(&self) -> Vec<&str> {
        let lines: Vec<&str> = self.0.lines().collect();
        let paragraphs: Vec<&[&str]> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with("dibs: "))
            .map(|(at, _)| {
                let explained = lines[at + 1..]
                    .iter()
                    .take_while(|l| l.starts_with("  "))
                    .count();
                &lines[at..=at + explained]
            })
            .collect();
        let Some((last, earlier)) = paragraphs.split_last() else {
            return Vec::new();
        };
        let explanation = earlier.iter().rev().find(|p| p.len() > 1);
        match explanation {
            Some(explanation) if last.len() == 1 => {
                explanation.iter().chain(last.iter()).copied().collect()
            }
            _ => last.to_vec(),
        }
    }
}

pub fn state_dir() -> PathBuf {
    Paths::from_env().batches().unwrap_or_default()
}

/// Where a step goes, resolved as the step's own call will resolve it. None when it names no
/// machine and several could take it, which a shared step is placed from and a measurement is
/// refused over.
pub fn machine_of(step: &Step, batch_on: Option<&MachineName>) -> Option<String> {
    let call = Call {
        on: step
            .on
            .as_deref()
            .map(MachineName::new)
            .or(batch_on.cloned()),
        ..Call::default()
    };
    let caller = Caller::default();
    match MachineCall::new(&call, &caller).and_then(|machine| machine.destination()) {
        Ok(Destination::Named(machine)) => Some(machine.to_string()),
        Ok(Destination::Unchosen) if call.on.is_none() => None,
        _ => Some(call.on.map_or_else(|| "?".into(), |on| on.to_string())),
    }
}

fn collect_old(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let keep = std::time::Duration::from_secs(14 * Span::DAY.0);
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > keep);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

pub struct Options {
    pub dry_run: bool,
    pub verbose: bool,
    /// The machine of every step that names none of its own.
    pub on: Option<MachineName>,
    /// The session the batch belongs to, which alone may stop it without `--anyone`.
    pub owner: Option<String>,
    /// The id the batch was given where it was sent from, for a batch driven on its machine.
    pub id: Option<BatchId>,
    /// Called once this process holds the batch as its driver.
    pub driving: Option<fn()>,
}

pub fn run(text: &str, opts: &Options) -> Result<i32, BatchError> {
    let steps = parse(text)?;
    let named: Vec<Option<String>> = steps
        .iter()
        .map(|s| machine_of(s, opts.on.as_ref()))
        .collect();
    if let Some((s, _)) = steps
        .iter()
        .zip(&named)
        .find(|(s, m)| m.is_none() && s.measures())
    {
        return Err(BatchError::Unplaced {
            step: s.name.clone(),
        });
    }
    let machines: Vec<String> = named
        .into_iter()
        .map(|m| m.unwrap_or_else(|| "?".into()))
        .collect();
    let id = opts
        .id
        .clone()
        .unwrap_or_else(BatchId::starting)
        .to_string();
    let batch = Batch {
        steps: &steps,
        machines: &machines,
    };
    let plan = batch.plan();
    if opts.dry_run {
        print!("batch (dry run), {} steps\n{plan}", steps.len());
        return Ok(0);
    }
    let root = state_dir();
    collect_old(&root);
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir).map_err(FileError::at(&dir))?;
    let _driving = Driver::claim(&dir).map_err(FileError::at(&dir))?;
    if let Some(owner) = &opts.owner {
        let _ = std::fs::write(dir.join("owner"), owner);
    }
    if let Some(driving) = opts.driving {
        driving();
    }
    eprint!(
        "dibs: batch {id}, {} steps. You are told when it ends; there is nothing to watch.\n{plan}",
        steps.len()
    );
    let states = vec![State::Waiting; steps.len()];
    let on = opts
        .on
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| std::env::var("DIBS_ON").ok().filter(|on| !on.is_empty()));
    let kept = Paused {
        text: text.to_string(),
        cwd: std::env::current_dir().unwrap_or_default(),
        on,
        check_in: std::env::var(CHECK_IN).ok(),
        low_priority: std::env::var(LOW_PRIORITY).is_ok_and(|v| v == "1"),
        image: std::env::var(IMAGE).ok(),
        verbose: opts.verbose,
        states: Vec::new(),
        seconds: 0,
    };
    drive(&kept, batch, &dir, &id, opts, states)
}

/// `dibs batch --follow` of a batch paused here: the jobs its steps let go of followed, then
/// the steps still to come run. A follow that finds another already driving it waits for that
/// one, then follows what it left.
pub fn resume(id: &BatchId) -> Result<i32, BatchError> {
    let dir = state_dir().join(id.as_str());
    let _driving = Driver::claim(&dir).map_err(FileError::at(&dir))?;
    let Some(paused) = std::fs::read(dir.join(Paused::FILE))
        .ok()
        .and_then(|json| serde_json::from_slice::<Paused>(&json).ok())
    else {
        if let Ok(summary) = std::fs::read_to_string(dir.join("summary")) {
            print!("{summary}");
        }
        return Ok(std::fs::read_to_string(dir.join(EXIT))
            .ok()
            .and_then(|code| code.trim().parse().ok())
            .unwrap_or(1));
    };
    std::env::set_current_dir(&paused.cwd).map_err(FileError::at(&paused.cwd))?;
    let steps = parse(&paused.text)?;
    let opts = Options {
        dry_run: false,
        verbose: paused.verbose,
        on: paused.on.as_deref().map(MachineName::new),
        owner: None,
        id: Some(id.clone()),
        driving: None,
    };
    let machines: Vec<String> = steps
        .iter()
        .map(|s| machine_of(s, opts.on.as_ref()).unwrap_or_else(|| "?".into()))
        .collect();
    let batch = Batch {
        steps: &steps,
        machines: &machines,
    };
    let following = paused
        .states
        .iter()
        .filter(|s| matches!(s, State::CheckedIn { .. }))
        .count();
    eprintln!(
        "dibs: batch {id}: following the {following} job(s) its steps let go of, then the steps still to come. You are told when it ends; there is nothing to watch."
    );
    let states = paused.states.clone();
    drive(&paused, batch, &dir, id.as_str(), &opts, states)
}

/// Where a batch's exit is kept once it has ended, for a follow that comes after.
const EXIT: &str = "exit";

/// Drives the steps from `states` until every one has ended, or until those running have ended
/// or checked in once one has checked in. `kept` is the batch as a pause keeps it, its states
/// aside.
fn drive(
    kept: &Paused,
    batch: Batch,
    dir: &Path,
    id: &str,
    opts: &Options,
    states: Vec<State>,
) -> Result<i32, BatchError> {
    let steps = batch.steps;
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let recipes: Vec<Option<Vec<Pending>>> = steps
        .iter()
        .map(|s| {
            if s.lock == StepKind::Recipe {
                s.recipe.as_ref().and_then(Job::of_batch_line)
            } else {
                None
            }
        })
        .collect();
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let resumed = dir.join(Paused::FILE).is_file();
    let mut driving = Driving {
        batch,
        recipes,
        dir,
        id,
        cwd,
        opts,
        states,
        stopped: false,
        paused: false,
        checked: false,
        unstopped: Vec::new(),
        on: kept.on.as_deref().map(MachineName::new),
        lost: None,
        cancelled: None,
        running: HashMap::new(),
        following: HashMap::new(),
        said_from: HashMap::new(),
        check_in: kept.check_in.clone(),
        // A follow given the flag itself runs the rest of the steps so too.
        low_priority: kept.low_priority || std::env::var(LOW_PRIORITY).is_ok_and(|v| v == "1"),
        // A batch started before its image was kept takes the follow's.
        image: kept
            .image
            .clone()
            .or_else(|| std::env::var(IMAGE).ok().filter(|image| !image.is_empty())),
        tx,
        rx,
    };
    driving.follow();
    driving.start();
    while driving.states.contains(&State::Running) {
        let end = driving.next_end()?;
        driving.ended(end);
        // A resumed batch is kept as it goes, before what waited on a step starts, so a driver
        // that dies runs nothing twice.
        if resumed {
            let seconds = kept.seconds + started.elapsed().as_secs();
            driving.keep(kept, seconds);
        }
        driving.start();
    }
    let seconds = kept.seconds + started.elapsed().as_secs();
    let checked_in = driving
        .states
        .iter()
        .any(|s| matches!(s, State::CheckedIn { .. }));
    if checked_in && driving.cancelled.is_none() {
        return Ok(driving.pause(kept, seconds));
    }
    if driving.cancelled.is_some() {
        for s in driving.states.iter_mut() {
            if *s == State::Waiting {
                *s = State::NotRun;
            }
        }
    }
    let followable = checked_in && !driving.unstopped.is_empty() && driving.keep(kept, seconds);
    if followable {
        eprintln!(
            "dibs: batch {id} was cancelled, but a job one of its steps let go of could not be stopped: dibs batch --follow {id} still follows it"
        );
    }
    let Driving {
        mut states,
        cancelled,
        ..
    } = driving;
    for s in states.iter_mut() {
        if matches!(s, State::Waiting | State::CheckedIn { .. }) {
            *s = State::NotRun;
        }
    }
    let report = batch.summary(id, &states, dir, seconds, cancelled.as_deref());
    let _ = std::fs::write(dir.join("summary"), &report);
    print!("{report}");
    let code = if cancelled.is_some() {
        i32::from(Exit::Cancelled.code())
    } else if states
        .iter()
        .all(|s| matches!(s, State::Done { exit: 0, .. }))
    {
        0
    } else {
        1
    };
    let _ = std::fs::write(dir.join(EXIT), format!("{code}\n"));
    if !followable {
        let _ = std::fs::remove_file(dir.join(Paused::FILE));
    }
    Ok(code)
}

/// How often a wait for a step to end looks for a cancellation.
const CANCEL_CHECK: Duration = Duration::from_millis(500);

/// How a step's command ended, and how long it ran.
struct StepEnded {
    step: usize,
    exit: i32,
    seconds: u64,
}

/// A batch's steps as they run: which wait, run and ended, and what stops the rest.
struct Driving<'a> {
    batch: Batch<'a>,
    recipes: Vec<Option<Vec<Pending>>>,
    dir: &'a Path,
    id: &'a str,
    cwd: String,
    opts: &'a Options,
    states: Vec<State>,
    stopped: bool,
    /// A step checked in, or lost its job, so no other starts until the batch is followed.
    paused: bool,
    /// A step checked in on this drive.
    checked: bool,
    /// The steps whose jobs a cancellation could not stop, which stay followable.
    unstopped: Vec<usize>,
    /// The machine of every step that names none, as the batch was started with.
    on: Option<MachineName>,
    /// The exit of a follow that never heard how its job ended, when no step checked in.
    lost: Option<i32>,
    cancelled: Option<String>,
    running: HashMap<usize, u32>,
    /// The steps waiting on a job they let go of, with how long each had run.
    following: HashMap<usize, (Hint, u64)>,
    /// Where each running step's stderr began, since a follow's is added to what it said before.
    said_from: HashMap<usize, u64>,
    /// `DIBS_CHECK_IN` for every step, as the batch was started with.
    check_in: Option<String>,
    low_priority: bool,
    /// `DIBS_IMAGE` for every step, as the batch was started with.
    image: Option<String>,
    tx: mpsc::Sender<StepEnded>,
    rx: mpsc::Receiver<StepEnded>,
}

impl Driving<'_> {
    /// Follows the job of every step that let go of one before the batch was paused, its
    /// output added to what the step wrote before.
    fn follow(&mut self) {
        let following: Vec<(usize, Hint, u64)> = (0..self.states.len())
            .filter_map(|i| match &self.states[i] {
                State::CheckedIn { hint, seconds } => Some((i, hint.clone(), *seconds)),
                _ => None,
            })
            .collect();
        for &(i, ..) in &following {
            self.states[i] = State::Running;
        }
        for (i, hint, seconds) in following {
            self.follow_step(i, hint, seconds);
        }
    }

    fn follow_step(&mut self, i: usize, hint: Hint, seconds: u64) {
        let line = Hint::line(&hint.follow);
        self.following.insert(i, (hint, seconds));
        self.spawn(i, &line, true, seconds);
    }

    /// Starts every step whose turn has come.
    fn start(&mut self) {
        let starting = self.batch.ready(&self.states, self.stopped || self.paused);
        for &i in &starting {
            self.states[i] = State::Running;
        }
        for i in starting {
            let line = self.batch.steps[i].line.clone();
            self.spawn(i, &line, false, 0);
        }
    }

    fn err(&self, i: usize) -> PathBuf {
        self.dir.join(format!("{}.err", self.batch.steps[i].name))
    }

    /// What step `i` said on stderr since it last started.
    fn said(&self, i: usize) -> String {
        let all = std::fs::read(self.err(i)).unwrap_or_default();
        let from = self.said_from.get(&i).copied().unwrap_or_default() as usize;
        String::from_utf8_lossy(all.get(from..).unwrap_or_default()).into_owned()
    }

    /// Runs `line` as step `i`, which has run `before` seconds already.
    fn spawn(&mut self, i: usize, line: &str, append: bool, before: u64) {
        let from = match append {
            true => std::fs::metadata(self.err(i)).map_or(0, |m| m.len()),
            false => 0,
        };
        self.said_from.insert(i, from);
        {
            let step = self.batch.steps[i].clone();
            let (out, err) = (
                self.dir.join(format!("{}.out", step.name)),
                self.dir.join(format!("{}.err", step.name)),
            );
            // Another machine's steps end while this one runs, and this machine never hears of it.
            let pending: Vec<Pending> = (0..self.batch.steps.len())
                .filter(|&j| {
                    j != i
                        && self.states[j] == State::Waiting
                        && self.batch.machines[j] == self.batch.machines[i]
                })
                .flat_map(|j| match &self.recipes[j] {
                    Some(jobs) => jobs
                        .iter()
                        .map(|p| Pending {
                            name: format!("{}: {}", self.batch.steps[j].name, p.name),
                            here: true,
                            ..p.clone()
                        })
                        .collect(),
                    None => vec![self.batch.steps[j].pending(true, &self.cwd)],
                })
                .collect();
            let batch = BatchStep::new(
                self.id,
                &step.name,
                i + 1,
                self.batch.steps.len(),
                None,
                &pending,
            );
            let tx = self.tx.clone();
            let verbose = self.opts.verbose;
            let t = Instant::now();
            let carried = Carried {
                check_in: self.check_in.as_deref(),
                low_priority: self.low_priority,
                image: self.image.as_deref(),
            };
            match StepGuard::spawn(line, &batch, self.on.as_ref(), carried) {
                Ok(mut guarded) => {
                    self.running.insert(i, guarded.id());
                    std::thread::spawn(move || {
                        let o = copy(
                            guarded.take_stdout(),
                            out,
                            append,
                            verbose.then(|| format!("{} ", step.name)),
                        );
                        let e = copy(
                            guarded.take_stderr(),
                            err,
                            append,
                            verbose.then(|| format!("{} ", step.name)),
                        );
                        // No code means a signal ended it, which the summary shows as killed.
                        let status = guarded.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                        let _ = (o.join(), e.join());
                        let _ = tx.send(StepEnded {
                            step: i,
                            exit: status,
                            seconds: before + t.elapsed().as_secs(),
                        });
                    });
                }
                Err(_) => {
                    let _ = self.tx.send(StepEnded {
                        step: i,
                        exit: 127,
                        seconds: before,
                    });
                }
            }
        }
    }

    /// Waits for a step to end, stopping the others once the batch is cancelled meanwhile.
    fn next_end(&mut self) -> Result<StepEnded, BatchError> {
        let mut received = self.rx.recv_timeout(CANCEL_CHECK);
        while matches!(received, Err(mpsc::RecvTimeoutError::Timeout)) {
            if self.cancelled.is_none() && self.dir.join("cancel").exists() {
                self.cancelled = Some("with dibs --kill".into());
                self.stopped = true;
                self.stop_running();
            }
            received = self.rx.recv_timeout(CANCEL_CHECK);
        }
        received.map_err(BatchError::Lost)
    }

    /// Records a step's end, and whether it stops the steps still waiting.
    fn ended(&mut self, end: StepEnded) {
        let StepEnded {
            step: i,
            exit,
            seconds,
        } = end;
        self.running.remove(&i);
        let said = self.said(i);
        let followed = self.following.remove(&i);
        if exit == Exit::CheckedIn.status()
            && self.cancelled.is_none()
            && let Some(hint) = Hint::read(&said)
        {
            // Nobody waits on a driver detached to its machine, so it follows the job itself.
            match self.opts.driving {
                Some(_) => {
                    self.states[i] = State::Running;
                    self.follow_step(i, hint, seconds);
                }
                None => {
                    self.states[i] = State::CheckedIn { hint, seconds };
                    self.paused = true;
                    self.checked = true;
                }
            }
            return;
        }
        if let Some((hint, before)) = followed
            && (self.cancelled.is_none() || self.unstopped.contains(&i))
            && !Driving::heard_end(&hint, &said, exit)
        {
            self.states[i] = State::CheckedIn {
                hint,
                seconds: before,
            };
            self.lost.get_or_insert(exit);
            self.paused = true;
            return;
        }
        self.states[i] = State::Done { exit, seconds };
        if exit == i32::from(Exit::Cancelled.code())
            && self.cancelled.is_none()
            && StepStderr(
                &std::fs::read_to_string(
                    self.dir.join(format!("{}.err", self.batch.steps[i].name)),
                )
                .unwrap_or_default(),
            )
            .cancelled()
        {
            self.cancelled = Some(format!("with dibs --kill on {}", self.batch.machines[i]));
            self.stop_running();
        }
        if self.cancelled.is_some() {
            self.stopped = true;
        } else if exit != 0 && !self.batch.steps[i].cont {
            self.batch.stop_after(i, &mut self.states);
        }
    }

    /// The batch as `dibs batch --follow` reads it: a step still following its job is kept
    /// checked in, and one cut off otherwise runs again.
    fn keep(&self, kept: &Paused, seconds: u64) -> bool {
        let states = (0..self.states.len())
            .map(|i| match (&self.states[i], self.following.get(&i)) {
                (State::Running, Some((hint, before))) => State::CheckedIn {
                    hint: hint.clone(),
                    seconds: *before,
                },
                (State::Running, None) => State::Waiting,
                (state, _) => state.clone(),
            })
            .collect();
        let paused = Paused {
            states,
            seconds,
            text: kept.text.clone(),
            cwd: kept.cwd.clone(),
            on: kept.on.clone(),
            check_in: kept.check_in.clone(),
            low_priority: self.low_priority,
            image: self.image.clone(),
            verbose: kept.verbose,
        };
        let file = self.dir.join(Paused::FILE);
        let part = file.with_extension("part");
        serde_json::to_vec(&paused).is_ok_and(|json| {
            std::fs::write(&part, json).is_ok() && std::fs::rename(&part, &file).is_ok()
        })
    }

    /// The batch left for `dibs batch --follow`, and what its steps said as they let go of their
    /// jobs, so whoever reads it can tell a job that is stuck from one that is slow.
    fn pause(&self, kept: &Paused, seconds: u64) -> i32 {
        let id = self.id;
        print!(
            "{}",
            self.batch
                .summary(id, &self.states, self.dir, seconds, None)
        );
        let stored = self.keep(kept, seconds);
        let mut said = String::new();
        let mut to_come = 0;
        for (i, state) in self.states.iter().enumerate() {
            match state {
                State::CheckedIn { .. } => {
                    let step = &self.batch.steps[i].name;
                    said.push_str(&format!(
                        "step {step}: {}",
                        StepStderr(&self.said(i)).check_in()
                    ));
                }
                State::Waiting => to_come += 1,
                _ => {}
            }
        }
        if !self.checked
            && let Some(exit) = self.lost
        {
            eprintln!(
                "dibs: batch {id} could not hear how a job it follows ended (exit {exit}), so it stops here with that job still running.\n  \
                 dibs batch --follow {id}  tries again, then runs the {to_come} step(s) still to come\n  \
                 dibs --kill {id}  stops the batch"
            );
            return exit;
        }
        eprint!("{said}");
        match stored {
            true => eprintln!(
                "dibs: batch {id} checked in: the jobs above run on without this call, and no step starts until it is followed.\n  \
                 dibs batch --follow {id}  waits for them, then runs the {to_come} step(s) still to come; it checks in again as a job does\n  \
                 dibs --kill {id}  stops the batch"
            ),
            false => eprintln!(
                "dibs: batch {id} checked in, but could not be kept in {} to be followed: the jobs above run on, and dibs --kill {id} stops them",
                self.dir.display()
            ),
        }
        Exit::CheckedIn.status()
    }

    /// Whether a follow heard how what it waited on ended: a job's trailer, or a batch's end.
    fn heard_end(hint: &Hint, said: &str, exit: i32) -> bool {
        let trailer = |job: &str| said.lines().any(|l| l.starts_with(&format!("job {job} ")));
        hint.job().is_some_and(trailer) || reached_its_job(exit)
    }

    /// Each step is its own process group, so the signal reaches the dibs call under it and that
    /// call's death reaches the machine, which stops the job and releases its lock. A job a step
    /// let go of outlives its follower, so it is stopped on its machine first.
    fn stop_running(&mut self) {
        let checked_in = self.states.iter().enumerate().filter_map(|(i, s)| match s {
            State::CheckedIn { hint, .. } => Some((i, hint)),
            _ => None,
        });
        let following = self.following.iter().map(|(&i, (hint, _))| (i, hint));
        let mut unstopped = Vec::new();
        for (i, hint) in following.chain(checked_in) {
            let Some(kill) = &hint.kill else { continue };
            let stopped = Itself::command()
                .args(kill)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !stopped {
                eprintln!(
                    "dibs: {} did not stop the job a step let go of; it may still run",
                    Hint::line(kill)
                );
                unstopped.push(i);
            }
        }
        self.unstopped.extend(unstopped);
        for &pid in self.running.values() {
            // SAFETY: signals the step's process group, which its guard leads.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
        }
    }
}

fn copy(
    from: Option<impl std::io::Read + Send + 'static>,
    to: PathBuf,
    append: bool,
    echo: Option<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let file = std::fs::File::options()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&to);
        let (Some(from), Ok(mut file)) = (from, file) else {
            return;
        };
        for line in BufReader::new(from).lines().map_while(Result::ok) {
            let _ = writeln!(file, "{line}");
            if let Some(prefix) = &echo {
                eprintln!("{prefix}{line}");
            }
        }
    })
}
