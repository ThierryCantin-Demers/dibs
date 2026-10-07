use super::{
    guard::StepGuard,
    parse::{BatchError, Step, StepKind, parse},
    plan::{Pending, pending_of, plan, step_env},
    summary::summary,
};
use crate::{
    call::{Destination, Driver, MachineCall},
    caller::Caller,
    cli::Call,
    execution::recipe_jobs,
    paths::{FileError, Paths},
};
use dibs_format::{Exit, MachineName, Span};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Waiting,
    Running,
    Done { exit: i32, seconds: u64 },
    NotRun,
}

/// The steps that may start now, lowest first. A step waits for everything it names, and for
/// its machine to have no other step of this batch on it: two steps on one machine overlapping
/// is the surprise the lock exists to prevent, and nothing is lost by running them in turn.
pub fn ready(steps: &[Step], machines: &[String], states: &[State], stopped: bool) -> Vec<usize> {
    if stopped {
        return Vec::new();
    }
    let index: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let mut busy: HashSet<&str> = states
        .iter()
        .enumerate()
        .filter(|(_, s)| **s == State::Running)
        .map(|(i, _)| machines[i].as_str())
        .collect();
    let mut out = Vec::new();
    for (i, s) in steps.iter().enumerate() {
        if states[i] != State::Waiting || busy.contains(machines[i].as_str()) {
            continue;
        }
        if s.after
            .iter()
            .all(|a| matches!(states[index[a.as_str()]], State::Done { .. }))
        {
            busy.insert(machines[i].as_str());
            out.push(i);
        }
    }
    out
}

/// The jobs a step's trailers named, in order, each with what its trailer says about the exit
/// and the build: `by=dibs` when dibs produced the exit, and `built=`, which is what says whether
/// a measurement ran on a fresh binary. A recipe step prints several.
pub fn jobs(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("job "))
        .filter_map(|r| {
            let mut words = r.split_whitespace();
            let id = words.next()?;
            let verdict: Vec<&str> = words
                .filter(|w| *w == "by=dibs" || w.starts_with("built="))
                .collect();
            Some(if verdict.is_empty() {
                id.to_string()
            } else {
                format!("{id} {}", verdict.join(" "))
            })
        })
        .collect()
}

pub fn state_dir() -> PathBuf {
    Paths::from_env().batches().unwrap_or_default()
}

/// Where a step goes, resolved as the step's own call will resolve it. None when it names no
/// machine and several could take it, which a shared step is placed from and a measurement is
/// refused over.
fn machine_of(step: &Step, batch_on: Option<&MachineName>) -> Option<String> {
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

pub fn batch_id() -> String {
    let stamp = Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    format!("{stamp}-{}", std::process::id())
}

pub fn collect_old(dir: &Path) {
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
    let id = batch_id();
    let plan = plan(&steps, &machines);
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
    eprint!(
        "dibs: batch {id}, {} steps. You are told when it ends; there is nothing to watch.\n{plan}",
        steps.len()
    );

    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let recipes: Vec<Option<Vec<Pending>>> = steps
        .iter()
        .map(|s| {
            if s.lock == StepKind::Recipe {
                s.recipe.as_ref().and_then(recipe_jobs)
            } else {
                None
            }
        })
        .collect();
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let mut driving = Driving {
        steps: &steps,
        machines: &machines,
        recipes,
        dir: &dir,
        id: &id,
        cwd,
        opts,
        states: vec![State::Waiting; steps.len()],
        stopped: false,
        cancelled: None,
        running: HashMap::new(),
        tx,
        rx,
    };
    driving.start();
    while driving.states.contains(&State::Running) {
        let end = driving.next_end()?;
        driving.ended(end);
        driving.start();
    }
    let Driving {
        mut states,
        cancelled,
        ..
    } = driving;
    for s in states.iter_mut() {
        if *s == State::Waiting {
            *s = State::NotRun;
        }
    }
    let report = summary(
        &id,
        &steps,
        &machines,
        &states,
        &dir,
        started.elapsed().as_secs(),
        cancelled.as_deref(),
    );
    let _ = std::fs::write(dir.join("summary"), &report);
    print!("{report}");
    Ok(if cancelled.is_some() {
        i32::from(Exit::Cancelled.code())
    } else if states
        .iter()
        .all(|s| matches!(s, State::Done { exit: 0, .. }))
    {
        0
    } else {
        1
    })
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
    steps: &'a [Step],
    machines: &'a [String],
    recipes: Vec<Option<Vec<Pending>>>,
    dir: &'a Path,
    id: &'a str,
    cwd: String,
    opts: &'a Options,
    states: Vec<State>,
    stopped: bool,
    cancelled: Option<String>,
    running: HashMap<usize, u32>,
    tx: mpsc::Sender<StepEnded>,
    rx: mpsc::Receiver<StepEnded>,
}

impl Driving<'_> {
    /// Starts every step whose turn has come.
    fn start(&mut self) {
        let starting = ready(self.steps, self.machines, &self.states, self.stopped);
        for &i in &starting {
            self.states[i] = State::Running;
        }
        for i in starting {
            let step = self.steps[i].clone();
            let (out, err) = (
                self.dir.join(format!("{}.out", step.name)),
                self.dir.join(format!("{}.err", step.name)),
            );
            let pending: Vec<Pending> = (0..self.steps.len())
                .filter(|&j| j != i && self.states[j] == State::Waiting)
                .flat_map(|j| {
                    let here = self.machines[j] == self.machines[i];
                    match &self.recipes[j] {
                        Some(jobs) => jobs
                            .iter()
                            .map(|p| Pending {
                                name: format!("{}: {}", self.steps[j].name, p.name),
                                here,
                                ..p.clone()
                            })
                            .collect(),
                        None => vec![pending_of(&self.steps[j], here, &self.cwd)],
                    }
                })
                .collect();
            let batch = step_env(self.id, &step.name, i + 1, self.steps.len(), &pending);
            let tx = self.tx.clone();
            let verbose = self.opts.verbose;
            let t = Instant::now();
            match StepGuard::spawn(&step.line, &batch, self.opts.on.as_ref()) {
                Ok(mut guarded) => {
                    self.running.insert(i, guarded.id());
                    std::thread::spawn(move || {
                        let o = copy(
                            guarded.take_stdout(),
                            out,
                            verbose.then(|| format!("{} ", step.name)),
                        );
                        let e = copy(
                            guarded.take_stderr(),
                            err,
                            verbose.then(|| format!("{} ", step.name)),
                        );
                        // No code means a signal ended it, which the summary shows as killed.
                        let status = guarded.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                        let _ = (o.join(), e.join());
                        let _ = tx.send(StepEnded {
                            step: i,
                            exit: status,
                            seconds: t.elapsed().as_secs(),
                        });
                    });
                }
                Err(_) => {
                    let _ = self.tx.send(StepEnded {
                        step: i,
                        exit: 127,
                        seconds: 0,
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
                self.running.values().for_each(|&pid| stop(pid));
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
        self.states[i] = State::Done { exit, seconds };
        if exit == i32::from(Exit::Cancelled.code())
            && self.cancelled.is_none()
            && was_cancelled(
                &std::fs::read_to_string(self.dir.join(format!("{}.err", self.steps[i].name)))
                    .unwrap_or_default(),
            )
        {
            self.cancelled = Some(format!("with dibs --kill on {}", self.machines[i]));
            self.running.values().for_each(|&pid| stop(pid));
        }
        if exit != 0 && (!self.steps[i].cont || self.cancelled.is_some()) {
            self.stopped = true;
        }
    }
}

/// A command may exit 76 of its own accord, so only dibs saying so makes it a cancellation.
pub fn was_cancelled(stderr: &str) -> bool {
    stderr.contains("was cancelled with dibs --kill")
        || stderr.lines().any(|l| {
            l.starts_with("job ")
                && l.contains(&format!("  exit {}  by=dibs", Exit::Cancelled.code()))
        })
}

/// A step is its own process group, so the signal reaches the dibs call under it and that call's
/// death reaches the machine, which stops the job and releases its lock.
pub fn stop(pid: u32) {
    // SAFETY: signals the step's process group, which its guard leads.
    unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
}

pub fn copy(
    from: Option<impl std::io::Read + Send + 'static>,
    to: PathBuf,
    echo: Option<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (Some(from), Ok(mut file)) = (from, std::fs::File::create(&to)) else {
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
