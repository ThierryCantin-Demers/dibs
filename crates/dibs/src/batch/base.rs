use super::{
    guard::StepGuard,
    parse::{BatchError, Step, StepKind, parse},
    plan::{Pending, pending_of, plan, step_env},
    summary::summary,
};
use crate::execution::recipe_jobs;
use dibs::{
    call::{Destination, Driver, MachineCall},
    caller::Caller,
    cli::Call,
    paths::Paths,
};
use dibs_format::{Exit, MachineName};
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

pub(crate) fn state_dir() -> PathBuf {
    Paths::from_env().batches().unwrap_or_default()
}

/// Where a step goes, resolved as the step's own call will resolve it. None when it names no
/// machine and several could take it, which a shared step is placed from and a measurement is
/// refused over.
pub(crate) fn machine_of(step: &Step) -> Option<String> {
    let call = Call {
        on: step.on.as_deref().map(MachineName::new),
        ..Call::default()
    };
    let caller = Caller::default();
    match MachineCall::new(&call, &caller).and_then(|machine| machine.destination()) {
        Ok(Destination::Named(machine)) => Some(machine.to_string()),
        Ok(Destination::Unchosen) if step.on.is_none() => None,
        _ => Some(step.on.clone().unwrap_or_else(|| "?".into())),
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

pub(crate) fn collect_old(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let keep = std::time::Duration::from_secs(14 * 86400);
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
}

pub fn run(text: &str, opts: &Options) -> Result<i32, BatchError> {
    let steps = parse(text)?;
    let named: Vec<Option<String>> = steps.iter().map(machine_of).collect();
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
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _driving = Driver::claim(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if let Ok(owner) = std::env::var("DIBS_BATCH_OWNER") {
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
    let mut states = vec![State::Waiting; steps.len()];
    let mut stopped = false;
    let mut cancelled: Option<String> = None;
    let mut running: HashMap<usize, u32> = HashMap::new();
    let (tx, rx) = mpsc::channel::<(usize, i32, u64)>();
    loop {
        let starting = ready(&steps, &machines, &states, stopped);
        for &i in &starting {
            states[i] = State::Running;
        }
        for i in starting {
            let step = steps[i].clone();
            let (out, err) = (
                dir.join(format!("{}.out", step.name)),
                dir.join(format!("{}.err", step.name)),
            );
            let pending: Vec<Pending> = (0..steps.len())
                .filter(|&j| j != i && states[j] == State::Waiting)
                .flat_map(|j| {
                    let here = machines[j] == machines[i];
                    match &recipes[j] {
                        Some(jobs) => jobs
                            .iter()
                            .map(|p| Pending {
                                name: format!("{}: {}", steps[j].name, p.name),
                                here,
                                ..p.clone()
                            })
                            .collect(),
                        None => vec![pending_of(&steps[j], here, &cwd)],
                    }
                })
                .collect();
            let batch = step_env(&id, &step.name, i + 1, steps.len(), &pending);
            let tx = tx.clone();
            let verbose = opts.verbose;
            let t = Instant::now();
            match StepGuard::spawn(&step.line, &batch) {
                Ok(mut guarded) => {
                    running.insert(i, guarded.id());
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
                        let _ = tx.send((i, status, t.elapsed().as_secs()));
                    });
                }
                Err(_) => {
                    let _ = tx.send((i, 127, 0));
                }
            }
        }
        if !states.contains(&State::Running) {
            break;
        }
        let (i, exit, seconds) = loop {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(done) => break done,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancelled.is_none() && dir.join("cancel").exists() {
                        cancelled = Some("with dibs --kill".into());
                        stopped = true;
                        running.values().for_each(|&pid| stop(pid));
                    }
                }
                Err(e) => return Err(e.to_string().into()),
            }
        };
        running.remove(&i);
        states[i] = State::Done { exit, seconds };
        if exit == i32::from(Exit::Cancelled.code())
            && cancelled.is_none()
            && was_cancelled(
                &std::fs::read_to_string(dir.join(format!("{}.err", steps[i].name)))
                    .unwrap_or_default(),
            )
        {
            cancelled = Some(format!("with dibs --kill on {}", machines[i]));
            running.values().for_each(|&pid| stop(pid));
        }
        if exit != 0 && (!steps[i].cont || cancelled.is_some()) {
            stopped = true;
        }
    }
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

/// A command may exit 76 of its own accord, so only dibs saying so makes it a cancellation.
pub(crate) fn was_cancelled(stderr: &str) -> bool {
    stderr.contains("was cancelled with dibs --kill")
        || stderr.lines().any(|l| {
            l.starts_with("job ")
                && l.contains(&format!("  exit {}  by=dibs", Exit::Cancelled.code()))
        })
}

/// A step is its own process group, so the signal reaches the dibs call under it and that call's
/// death reaches the machine, which stops the job and releases its lock.
pub(crate) fn stop(pid: u32) {
    // SAFETY: signals the step's process group, which its guard leads.
    unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
}

pub(crate) fn copy(
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
