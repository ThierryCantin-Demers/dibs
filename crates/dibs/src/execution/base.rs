use super::pins;
use super::{
    jobs::{BACKEND, JobOutcome, Jobs, Reported, Request},
    pins::{pin_spec, pins_of},
    record::{batch_of_caller, fetch_artifacts, measured_summary},
    refs::{Arm, arms, sent_from, short, sides},
    schedule::{Job, jobs_of, schedule},
    sweep::{sweep_points, sweep_run},
};
use crate::{
    artifacts, batch, gitdeps, provenance,
    recipe::{self, Lock, Manifest, RecipeError, Resolved, resolve},
    records::{affinity_get, affinity_set, now_secs, pinned, write_record},
    worktree,
};
use dibs::{
    call::{CallError, Destination, RecipeJob},
    cli::{RecipeCall, ShellWord},
};
use dibs_format::{
    Alias, ArmRecord, Exit, MachineName, Outcome, Pairs, ProcedureStep, RunRecord, RunVerb,
    StepRecord,
};
use std::{collections::BTreeMap, fmt, path::Path, process::ExitCode};

/// What stops a recipe run, said as its `Display` in full, and the exit it ends with.
#[derive(Debug)]
pub(crate) enum RunError {
    /// Refused here: exit 2.
    Refused(String),
    /// A call the run made failed. Its exit passes on, so an unreachable machine reads as 69 to
    /// whoever ran this rather than as a refusal.
    Call { exit: i32, why: String },
    /// A call refused before it was sent, in its own words.
    Unsent(CallError),
}

impl RunError {
    pub(crate) fn call(exit: i32, why: String) -> RunError {
        RunError::Call { exit, why }
    }

    pub(crate) fn exit(&self) -> u8 {
        let refused = Exit::Refused.code();
        let exit = match self {
            RunError::Refused(_) => return refused,
            RunError::Call { exit, .. } => *exit,
            RunError::Unsent(e) => e.exit(),
        };
        u8::try_from(exit)
            .ok()
            .filter(|c| *c != 0)
            .unwrap_or(refused)
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Refused(why) | RunError::Call { why, .. } => writeln!(f, "dibs: {why}"),
            RunError::Unsent(e) => e.fmt(f),
        }
    }
}

impl From<CallError> for RunError {
    fn from(e: CallError) -> RunError {
        RunError::Unsent(e)
    }
}

impl From<String> for RunError {
    fn from(why: String) -> RunError {
        RunError::Refused(why)
    }
}

impl From<&str> for RunError {
    fn from(why: &str) -> RunError {
        RunError::Refused(why.to_string())
    }
}

impl From<batch::BatchError> for RunError {
    fn from(e: batch::BatchError) -> RunError {
        RunError::Refused(e.to_string())
    }
}

impl From<RecipeError> for RunError {
    fn from(e: RecipeError) -> RunError {
        RunError::Refused(e.to_string())
    }
}

/// What is about to be prepared, for the person reading along.
pub(crate) fn preparing(
    repo: &str,
    arm: &Arm,
    local: Option<&worktree::Local>,
    dir: &Path,
) -> String {
    match (&arm.checkout, local) {
        (Some(c), _) => format!(
            "{repo} at {}{}",
            short(&c.sha),
            sent_from(c, arm.note.as_deref(), "this computer")
        ),
        (None, Some(l)) => format!(
            "{repo} from {} ({})",
            dir.display(),
            if l.dirty {
                "uncommitted changes included"
            } else {
                "clean"
            }
        ),
        (None, None) => format!(
            "{repo}@{}{}",
            arm.fetch.as_deref().unwrap_or_default(),
            arm.note
                .as_ref()
                .map(|n| format!(", {n}"))
                .unwrap_or_default()
        ),
    }
}

/// A tree on its way to the machine, and what it became there.
pub(crate) struct Tree {
    pub(crate) token: String,
    pub(crate) script: String,
    pub(crate) gitdbs: Vec<gitdeps::Db>,
    pub(crate) local: Option<worktree::Local>,
    pub(crate) prepared: Option<worktree::Prepared>,
}

pub(crate) fn run_recipe(args: RecipeCall) -> Result<ExitCode, RunError> {
    let points = sweep_points(&args);
    if points.len() > 1 {
        return sweep_run(&args, &points);
    }
    // One point is the ordinary call with its values filled in, not a batch of one.
    let args = RecipeCall {
        params: points.into_iter().next().unwrap_or_default(),
        ..args
    };

    let sides = sides(args.reference.as_deref())?;
    let resolved = resolve(&args)?;
    let mut arms = arms(&sides, &resolved.dir, &resolved.repo_name)?;
    let mut pins = pins_of(&args, &resolved.repo_name, &resolved.dir, &arms)?;
    let local: Vec<bool> = arms.iter().map(|a| a.fetch.is_none()).collect();
    let pin_specs = args
        .pins
        .iter()
        .zip(&pins)
        .map(|(spec, p)| pin_spec(spec).map(|s| (s.repo.to_string(), p.local.is_some())))
        .collect::<Result<Vec<_>, _>>()?;
    let calls = jobs_of(&resolved, &sides, &local, args.reps, &pin_specs);
    let Resolved {
        dir,
        repo_name,
        verb,
        name,
        rec,
        label,
        step_labels,
        shell_reason,
        params,
        tree_fresh,
    } = resolved;
    let (name, rec) = (name.as_str(), &rec);
    let fingerprint = rec.fingerprint();
    let patched: Option<std::collections::BTreeSet<String>> = (!pins.is_empty()).then(|| {
        pins.iter()
            .flat_map(|p| p.sources.values().flatten().cloned())
            .collect()
    });
    let jobs = schedule(&local, &rec.steps, args.reps);
    let compared = arms.len() > 1;
    let order = |jobs: &[Job]| -> String {
        let mut reps: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
        for j in jobs {
            if let Job::Step {
                arm, rep: Some(r), ..
            } = j
            {
                let names = reps.entry(*r).or_default();
                if names.last() != Some(&arms[*arm].name.as_str()) {
                    names.push(&arms[*arm].name);
                }
            }
        }
        reps.values()
            .map(|n| n.join(" "))
            .collect::<Vec<_>>()
            .join(" | ")
    };

    if args.dry_run {
        // Asked what the real run asks before it builds, so a card or a series it would refuse
        // is refused here, where someone reads the device line before measuring.
        if rec.steps.iter().any(|s| s.lock == Lock::Exclusive) || args.device.is_some() {
            let backend = Jobs::on(destination(rec, &repo_name, name)?);
            if refused_before_building(&backend, rec, &step_labels, &args) {
                return Ok(ExitCode::from(Exit::Refused.code()));
            }
        }
        println!("label       {label}");
        println!("recipe      {name}  ({fingerprint})");
        println!("isolation   {:?}", rec.isolation);
        for arm in &arms {
            let head = if compared {
                format!("arm         {}  ", arm.name)
            } else {
                "ref         ".to_string()
            };
            match (&arm.fetch, &arm.checkout) {
                (None, Some(c)) => println!(
                    "{head}{}{}",
                    c.sha,
                    sent_from(c, arm.note.as_deref(), &c.dir.display().to_string())
                ),
                (None, None) => {
                    let l = worktree::local(&dir)?;
                    println!("{head}local {} from {}", l.content, dir.display());
                    println!(
                        "            {}",
                        if l.dirty {
                            "uncommitted changes are included and are in that hash"
                        } else {
                            "clean, so this is the commit as it stands"
                        }
                    );
                }
                (Some(r), _) => println!(
                    "{head}{r}{}",
                    arm.note
                        .as_ref()
                        .map(|n| format!(", {n}"))
                        .unwrap_or_default()
                ),
            }
        }
        for p in &pins {
            match (&p.checkout, &p.local) {
                (Some(c), _) => println!(
                    "pin         {}  {} {}{}",
                    p.repo,
                    p.reference,
                    c.sha,
                    sent_from(c, p.note.as_deref(), &c.dir.display().to_string())
                ),
                (None, Some(l)) => println!(
                    "pin         {}  local {} from {}",
                    p.repo,
                    l.content,
                    p.dir.display()
                ),
                (None, None) => println!("pin         {}  {}", p.repo, p.reference),
            }
            for (source, names) in &p.sources {
                println!(
                    "            patches {source}: {}",
                    names.iter().cloned().collect::<Vec<_>>().join(" ")
                );
            }
        }
        if compared || args.reps > 1 {
            println!("measured    {}", order(&jobs));
        }
        // Which card, printed whether or not one was named: a dry run is where someone checks
        // they are about to measure the thing they mean to, and "no card named" is the answer
        // that most needs saying, because that run is the one nobody can repeat.
        match &args.device {
            Some(d) => println!("device      {d}"),
            None => println!("device      none named, so the runtime picks and a repeat is luck"),
        }
        for (k, v) in &params {
            println!("param       {k} = {v}");
        }
        for v in &rec.fresh {
            println!("fresh       {v}, a value of its own each run");
        }
        if !rec.artifacts.is_empty() {
            println!("artifacts   {}", rec.artifacts.join(" "));
            println!(
                "            {}",
                match &args.artifacts_to {
                    Some(d) => format!("copied into {d}"),
                    None =>
                        "kept beside each job's log; --artifacts <dir> copies them out".to_string(),
                }
            );
        }
        for (i, s) in rec.steps.iter().enumerate() {
            println!("step {}      [{:?}] {}", i + 1, s.lock, s.run);
            for (k, v) in &s.env {
                println!("            env {k}={v}");
            }
            println!("            label {}", step_labels[i]);
        }
        return Ok(ExitCode::SUCCESS);
    }

    // A measurement's claim on the repo's build cache is the one that sticks, since it could not
    // move; a pinned call claims nothing, since the machines report the caches it leaves.
    let backend = Jobs::on(destination(rec, &repo_name, name)?);
    if let Some(m) = backend.machine.as_ref().filter(|_| !pinned()) {
        affinity_set(&repo_name, m.as_str());
    }
    if refused_before_building(&backend, rec, &step_labels, &args) {
        return Ok(ExitCode::from(Exit::Refused.code()));
    }

    // Trees are prepared under the shared lock, so no fetch or checkout runs inside a hold.
    let own_batch = batch::batch_id();
    // The fingerprint is of the bound recipe: two runs sharing it are the same work.
    let env_of = |k: usize| RecipeJob {
        batch: batch::recipe_env(&own_batch, &calls, k),
        fingerprint: Some(fingerprint.clone()),
    };
    let mut announce = |text: &str| announce_prepared(text);

    // The pinned trees go first: the patch names where they landed.
    let mut pinned = Vec::with_capacity(pins.len());
    for (k, p) in pins.iter_mut().enumerate() {
        let env = env_of(k);
        let setup = Request {
            label: &calls[k].label,
            lock: Lock::Shared,
            device: None,
            job: &env,
            max: None,
            new_series: false,
        };
        let fresh = Manifest::load_any(&p.dir, &p.repo)?;
        let lock = p.checkout.as_mut().and_then(|c| c.lock.take());
        let from = p
            .checkout
            .as_ref()
            .map_or(p.dir.as_path(), |c| c.dir.as_path());
        let TreeScript { script, gitdbs } = TreeSpec {
            dir: from,
            repo_name: &p.repo,
            reference: &p.reference,
            local: p.local.as_ref(),
            signature: "",
            token: &new_token(),
            slot: 0,
            nest: None,
            fresh: fresh.tree_fresh(),
        }
        .script();
        let text = match &p.local {
            Some(l) => {
                match &p.checkout {
                    Some(c) => eprintln!(
                        "dibs: pinning {}@{} at {}{}",
                        p.repo,
                        p.reference,
                        short(&c.sha),
                        sent_from(c, p.note.as_deref(), "this computer")
                    ),
                    None => eprintln!(
                        "dibs: pinning {} from {} ({})",
                        p.repo,
                        p.dir.display(),
                        if l.dirty {
                            "uncommitted changes included"
                        } else {
                            "clean"
                        }
                    ),
                }
                let Reported { outcome: out, text } =
                    sync_prepared(&backend, from, &script, &l.key, &setup, &mut announce);
                drop(lock);
                if !text.contains("DIBS-READY") || out.status != 0 {
                    return Err(RunError::call(
                        out.status,
                        format!(
                            "could not send the pinned {} from {} (exit {})",
                            p.repo,
                            from.display(),
                            out.status
                        ),
                    ));
                }
                text
            }
            None => {
                eprintln!("dibs: pinning {}@{}", p.repo, p.reference);
                let Reported { outcome: out, text } = backend.run_capture(&setup, &script);
                if out.status != 0 {
                    return Err(RunError::call(
                        out.status,
                        format!(
                            "could not prepare the pinned {}@{} (exit {})",
                            p.repo, p.reference, out.status
                        ),
                    ));
                }
                text
            }
        };
        send_missing_gitdbs(&backend, &text, &gitdbs);
        pinned.push(worktree::parse(&text)?);
    }
    let nest = (!pins.is_empty()).then(|| {
        worktree::Nest::new(pins::config(
            &pins
                .iter()
                .zip(&pinned)
                .map(|(p, t)| pins::PinnedTree {
                    worktree: t.worktree.clone(),
                    crates: p.crates.clone(),
                    sources: p.sources.clone(),
                })
                .collect::<Vec<_>>(),
        ))
    });

    let signature = rec
        .steps
        .iter()
        .find_map(|st| worktree::build_signature(&st.run))
        .unwrap_or_default();
    let mut slot = 0;
    let mut trees = Vec::with_capacity(arms.len());
    if compared {
        eprintln!(
            "dibs: comparing {} arms of {repo_name}, measured {}",
            arms.len(),
            order(&jobs)
        );
    }
    for arm in &arms {
        let token = new_token();
        let local = match arm.fetch {
            None => Some(arm.local(&dir)?),
            Some(_) => None,
        };
        let lead = if compared {
            format!("  {}: ", arm.name)
        } else {
            "dibs: preparing ".to_string()
        };
        eprintln!("{lead}{}", preparing(&repo_name, arm, local.as_ref(), &dir));
        let reference = arm.fetch.as_deref().unwrap_or("local");
        let TreeScript { script, gitdbs } = TreeSpec {
            dir: arm.dir(&dir),
            repo_name: &repo_name,
            reference,
            local: local.as_ref(),
            signature: &signature,
            token: &token,
            slot,
            nest: nest.as_ref(),
            fresh: &tree_fresh,
        }
        .script();
        slot += usize::from(arm.fetch.is_some());
        trees.push(Tree {
            token,
            script,
            gitdbs,
            local,
            prepared: None,
        });
    }

    let mut checkout_locks: Vec<Option<std::fs::File>> = arms
        .iter_mut()
        .map(|a| a.checkout.as_mut().and_then(|c| c.lock.take()))
        .collect();
    let what = |arm: usize| match &arms[arm].fetch {
        Some(r) => format!("{repo_name}@{r}"),
        None => format!("{repo_name} from {}", arms[arm].dir(&dir).display()),
    };
    let tag = |arm: usize, rep: Option<u32>| {
        let mut t = String::new();
        if compared {
            t += &format!("{}: ", arms[arm].name);
        }
        t + &match rep.filter(|_| args.reps > 1) {
            Some(r) => format!("rep {r}/{}, ", args.reps),
            None => String::new(),
        }
    };

    let mut steps = Vec::new();
    let mut failed = None;
    let mut state = Pairs::default();
    for (k, job) in jobs
        .iter()
        .copied()
        .enumerate()
        .map(|(i, j)| (i + pins.len(), j))
    {
        if failed.is_some() {
            break;
        }
        let env = env_of(k);
        let setup = Request {
            label: &calls[k].label,
            lock: Lock::Shared,
            // Preparing a worktree touches no GPU, so pinning it would only make the setup fail
            // on a machine whose card has been pulled.
            device: None,
            job: &env,
            max: None,
            new_series: false,
        };
        let (arm, step, rep, fold) = match job {
            Job::Send(a) => {
                let t = &mut trees[a];
                let key = &t.local.as_ref().expect("a sent tree is local").key;
                let Reported { outcome: out, text } = sync_prepared(
                    &backend,
                    arms[a].dir(&dir),
                    &t.script,
                    key,
                    &setup,
                    &mut announce,
                );
                checkout_locks[a] = None;
                if !text.contains("DIBS-READY") {
                    return Err(RunError::call(
                        out.status,
                        format!("could not prepare {} (exit {})", what(a), out.status),
                    ));
                }
                if out.status != 0 {
                    return Err(RunError::call(
                        out.status,
                        format!(
                            "sending {} failed (exit {})",
                            arms[a].dir(&dir).display(),
                            out.status
                        ),
                    ));
                }
                t.prepared = Some(worktree::parse(&text)?);
                send_missing_gitdbs(&backend, &text, &t.gitdbs);
                continue;
            }
            Job::Setup(a) => {
                let t = &mut trees[a];
                let Reported { outcome: out, text } = backend.run_capture(&setup, &t.script);
                if out.status != 0 {
                    return Err(RunError::call(
                        out.status,
                        format!("could not prepare {} (exit {})", what(a), out.status),
                    ));
                }
                announce(&text);
                t.prepared = Some(worktree::parse(&text)?);
                send_missing_gitdbs(&backend, &text, &t.gitdbs);
                continue;
            }
            Job::Step {
                arm,
                step,
                rep,
                setup,
            } => (arm, step, rep, setup),
        };
        let lock = rec.steps[step].lock;
        let req = Request {
            label: &step_labels[step],
            lock,
            device: args.device.as_deref(),
            job: &env,
            max: args.max,
            new_series: args.new_series,
        };
        // One cache per repo, exported rather than left to each recipe to remember. The output
        // needs no file of its own: dibs keeps every job's log under its job id, and a path named
        // after the label would be shared by two runs of one recipe.
        let t = &mut trees[arm];
        let fresh = match args.reps {
            1 => t.token.clone(),
            _ => format!("{}-r{}", t.token, rep.unwrap_or(1)),
        };
        let run = step_command(rec, step, &t.token, &fresh, args.anyway);
        let run = match (&patched, worktree::build_signature(&rec.steps[step].run)) {
            (Some(names), Some(_)) if lock == Lock::Shared => pins::checked(&run, names),
            _ => run,
        };
        let record = |out: &JobOutcome, report: &str| StepRecord {
            arm: compared.then(|| arms[arm].name.clone()),
            rep: rep.filter(|_| args.reps > 1),
            artifacts: artifacts::kept(report),
            ..out.step_record(lock)
        };
        if fold {
            eprintln!(
                "dibs: {}step {}/{} [{lock:?}], with the setup ahead of it",
                tag(arm, rep),
                step + 1,
                rec.steps.len()
            );
            let command = worktree::ahead(&t.script, worktree::Then::Step, &rec.steps[step].run)
                + &format!("{{ {run}; }}");
            let Reported { outcome: out, text } =
                backend.run_reporting(&req, &command, &mut announce);
            if !text.contains("DIBS-READY") && !text.contains("DIBS-HELD") {
                return Err(RunError::call(
                    out.status,
                    format!("could not prepare {} (exit {})", what(arm), out.status),
                ));
            }
            t.prepared = Some(worktree::parse(&text)?);
            send_missing_gitdbs(&backend, &text, &t.gitdbs);
            if text.contains("DIBS-READY") {
                steps.push(record(&out, &text));
                if out.status != 0 {
                    failed = Some(out.status);
                }
                continue;
            }
        }
        let p = t
            .prepared
            .as_ref()
            .expect("an arm is prepared before its steps run");
        let cd = format!(
            "cd {} && export CARGO_TARGET_DIR={} && {{ {run}; }}",
            sh(&p.worktree),
            sh(&p.target)
        );
        eprintln!(
            "dibs: {}step {}/{} [{lock:?}]",
            tag(arm, rep),
            step + 1,
            rec.steps.len()
        );
        // The step says which lock it wants, where it can be reviewed, instead of a compile
        // being invisible inside a script that holds the machine exclusively.
        let Reported {
            outcome: out,
            text: report,
        } = backend.run_reporting(&req, &cd, &mut |_| {});
        // The machine has said why; a refusal is not a run, so it leaves no record.
        if report.lines().any(|l| l == "DIBS-REFUSED") {
            return Ok(ExitCode::from(Exit::TargetRebuilt.code()));
        }
        let read = provenance::state_of(&report);
        if !read.is_empty() {
            state = read;
        }
        steps.push(record(&out, &report));
        if out.status != 0 {
            failed = Some(out.status);
        }
    }
    fetch_artifacts(&backend, &steps, args.artifacts_to.as_deref(), compared);

    let pinned_revisions: Vec<(String, String)> =
        pinned.iter().flat_map(|p| p.revisions.clone()).collect();
    let prepared = |a: usize| trees[a].prepared.as_ref();
    let revisions_of = |a: usize| -> Vec<(String, String)> {
        prepared(a)
            .map(|p| p.revisions.clone())
            .unwrap_or_default()
            .into_iter()
            .chain(pinned_revisions.iter().cloned())
            .collect()
    };
    if compared || args.reps > 1 {
        eprint!("{}", measured_summary(&arms, &steps, &revisions_of));
    }
    let record = RunRecord {
        when: now_secs(),
        label,
        repo: repo_name.clone(),
        variant: worktree::variant(&dir, &repo_name),
        // shell borrows Build's machinery but is not a build, and a record that says
        // otherwise is a record that misleads whoever reads it later.
        verb: match shell_reason {
            Some(_) => RunVerb::Shell,
            None => verb.run_verb(),
        },
        recipe: name.to_string(),
        fingerprint,
        reason: shell_reason.clone(),
        procedure: rec
            .steps
            .iter()
            // With what it exported, since a recipe in local config cannot be recovered by
            // checking out a ref and an environment variable changes what was measured.
            .map(|st| {
                let exports: String = st
                    .env
                    .iter()
                    .map(|(k, v)| format!("export {k}={}; ", sh(v)))
                    .collect();
                ProcedureStep {
                    lock: st.lock,
                    run: format!("{exports}{}", st.run),
                }
            })
            .collect(),
        params,
        isolation: format!("{:?}", rec.isolation).to_lowercase(),
        needs: None,
        backend: BACKEND.into(),
        device: args.device.as_deref().map(Alias::from),
        machine: backend.machine.clone(),
        // Read on the machine, from the tree that was actually built, rather than from a
        // checkout here that may be at a different commit entirely.
        revisions: match compared {
            false => revisions_of(0).into(),
            true => Pairs::default(),
        },
        seeded: prepared(0)
            .filter(|_| !compared)
            .and_then(|p| p.seeded.clone()),
        refs: compared.then(|| args.reference.clone()).flatten(),
        arms: match compared {
            false => Vec::new(),
            true => arms
                .iter()
                .enumerate()
                .map(|(a, arm)| ArmRecord {
                    name: arm.name.clone(),
                    fetched: arm.fetch.clone(),
                    revisions: revisions_of(a).into(),
                    seeded: prepared(a).and_then(|p| p.seeded.clone()),
                })
                .collect(),
        },
        reps: args.reps,
        batch: batch_of_caller(),
        anyway: args.anyway,
        new_series: args.new_series,
        fresh: Vec::from_iter(fresh_values(rec, &trees[0].token)).into(),
        state,
        outcome: Some(Outcome::of_steps(&steps)),
        steps,
    };
    write_record(&record)?;

    Ok(match failed {
        Some(c) => ExitCode::from(c.clamp(1, 255) as u8),
        None => ExitCode::SUCCESS,
    })
}

/// A sweep is a batch of ordinary calls, which is what makes it one wake and one summary rather
/// than one per point. They run in sequence because they share a worktree and its build cache.
/// A recipe with any exclusive step is a measurement, and a measurement goes where it is told:
/// its history keys on the machine, and bindings that would make moving one safe do not exist
/// yet. So only a wholly shared recipe, which is every build and test, is placed.
pub(crate) fn destination(
    rec: &recipe::Recipe,
    repo_name: &str,
    name: &str,
) -> Result<Option<MachineName>, RunError> {
    if rec.steps.iter().all(|s| s.lock == Lock::Shared) {
        let placed = Jobs::placed(affinity_get(repo_name).as_deref(), Some(repo_name))?;
        return Ok(placed.machine);
    }
    match Jobs::destination()? {
        Destination::Named(m) => Ok(Some(m)),
        Destination::Unnamed => Ok(None),
        Destination::Unchosen => Err(format!(
            "{name} measures, and a measurement names its machine: its series belongs to the machine it\n  \
             ran on. Give --on <machine>, or export DIBS_ON; dibs --machines lists them."
        )
        .into()),
    }
}

/// Whether the wrapper refuses a measured step, or the first step when a card is named, on what it
/// decides without the machine. A shared recipe's card is otherwise checked by the first step that
/// carries it, after the tree and its dependencies have been sent.
pub(crate) fn refused_before_building(
    backend: &Jobs,
    rec: &recipe::Recipe,
    step_labels: &[String],
    args: &RecipeCall,
) -> bool {
    let exclusive: Vec<usize> = (0..rec.steps.len())
        .filter(|&i| rec.steps[i].lock == Lock::Exclusive)
        .collect();
    let asked = match exclusive.is_empty() && args.device.is_some() {
        true => vec![0],
        false => exclusive,
    };
    for i in asked {
        let req = Request {
            label: &step_labels[i],
            lock: rec.steps[i].lock,
            device: args.device.as_deref(),
            job: &RecipeJob::default(),
            max: None,
            new_series: args.new_series,
        };
        if !backend.preflight(&req) {
            return true;
        }
    }
    false
}

/// What step `i` runs in its tree. A build claims the target for this tree, and a measurement
/// after one refuses a target some other tree has built into since, unless told `anyway`.
/// `token` is the tree's prepare, whose package list a build records; `fresh` is this run's.
pub(crate) fn step_command(
    rec: &recipe::Recipe,
    i: usize,
    token: &str,
    fresh: &str,
    anyway: bool,
) -> String {
    let step = &rec.steps[i];
    let run = match (worktree::build_signature(&step.run), step.lock) {
        (Some(_), Lock::Shared) => worktree::claiming(&worktree::recording(&step.run, token)),
        (Some(_), Lock::Exclusive) => worktree::recording(&step.run, token),
        (None, _) => step.run.clone(),
    };
    let built = rec.steps[..i]
        .iter()
        .any(|s| s.lock == Lock::Shared && worktree::build_signature(&s.run).is_some());
    let run = match step.lock {
        Lock::Exclusive if built && !anyway => worktree::checked(&provenance::stated(&run)),
        Lock::Exclusive => provenance::stated(&run),
        Lock::Shared => run,
    };
    // Exported rather than prefixed onto the command, so it reaches a pipeline or a loop in the
    // step as well as the first word of it.
    let exports: String = fresh_values(rec, fresh)
        .iter()
        .chain(&step.env)
        .map(|(k, v)| format!("export {k}={}; ", sh(v)))
        .collect();
    artifacts::collecting(&format!("{exports}{run}"), &rec.artifacts)
}

/// One value per run for each of the recipe's `fresh` variables, the same in every step of it.
pub(crate) fn fresh_values(rec: &recipe::Recipe, token: &str) -> BTreeMap<String, String> {
    rec.fresh
        .iter()
        .map(|v| (v.clone(), format!("dibs-{token}")))
        .collect()
}

/// What the machine needs to know to prepare one tree.
pub(crate) struct TreeSpec<'a> {
    pub(crate) dir: &'a Path,
    pub(crate) repo_name: &'a str,
    pub(crate) reference: &'a str,
    pub(crate) local: Option<&'a worktree::Local>,
    pub(crate) signature: &'a str,
    pub(crate) token: &'a str,
    pub(crate) slot: usize,
    pub(crate) nest: Option<&'a worktree::Nest>,
    pub(crate) fresh: &'a [String],
}

/// The script that prepares a tree on the machine, and the git databases it may need sent.
pub(crate) struct TreeScript {
    pub(crate) script: String,
    pub(crate) gitdbs: Vec<gitdeps::Db>,
}

impl TreeSpec<'_> {
    pub(crate) fn script(&self) -> TreeScript {
        let lock = lockfile(self.dir, self.local.is_none().then_some(self.reference));
        let gitdbs = gitdeps::local(
            &gitdeps::cargo_home(),
            &gitdeps::pinned(lock.as_deref().unwrap_or("")),
        );
        let (repo, nest, fresh) = (self.repo_name, self.nest, self.fresh);
        let script =
            worktree::packages_script(lock.as_deref().unwrap_or(""), self.signature, self.token)
                + &match self.local {
                    Some(l) => worktree::setup_local_script(repo, &l.key, &l.content, nest, fresh),
                    None => worktree::setup_script(repo, self.reference, self.slot, nest, fresh),
                }
                + &gitdeps::check_script(&gitdbs);
        TreeScript { script, gitdbs }
    }
}

/// The lockfile of the tree here, or of a ref in its history.
pub(crate) fn lockfile(dir: &Path, reference: Option<&str>) -> Option<String> {
    match reference {
        None => std::fs::read_to_string(dir.join("Cargo.lock")).ok(),
        Some(r) => std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["show", &format!("{r}:Cargo.lock")])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()),
    }
}

/// Unique per invocation, and what a build's package list is staged under until it succeeds.
pub(crate) fn new_token() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

pub(crate) fn send_missing_gitdbs(backend: &Jobs, text: &str, gitdbs: &[gitdeps::Db]) {
    let gitdeps::Missing {
        gitdb: Some(remote),
        gone,
    } = gitdeps::missing(text, gitdbs)
    else {
        return;
    };
    for db in gone {
        eprintln!(
            "dibs: sending {} at {:.8}, which the machine does not have and may not be able to fetch",
            db.name, db.commit
        );
        if let Err(e) = sync_gitdb(backend, &db.path, &format!("{remote}/{}", db.name)) {
            eprintln!("dibs: {e}; the build will try to fetch it itself");
        }
    }
}

/// Prepares the worktree and sends the local tree into it, as one job under one lock.
///
/// `--no-times` is the load-bearing option and it is not tidiness. rsync's `-a` implies `-t`,
/// which is right for a transfer and wrong for sources about to be compiled: files that arrive
/// carrying an older mtime than the artifacts already beside them leave cargo with nothing to
/// do, so the build finishes in a fraction of a second and the previous binary is what gets
/// measured. It reads exactly like a fast incremental build. `--checksum` is what makes
/// dropping `-t` affordable, because without it every destination mtime differs on the next
/// pass and the whole tree goes again each time.
///
/// The filter follows the repo's own ignore rules, so a target directory or an editor's
/// droppings never make the trip, and `--delete` means a file deleted locally stops existing
/// there too rather than going on compiling.
pub(crate) fn sync_prepared(
    backend: &Jobs,
    from: &Path,
    setup: &str,
    key: &str,
    req: &Request,
    on_report: &mut dyn FnMut(&str),
) -> Reported {
    let before = worktree::ahead(
        setup,
        worktree::Then::Transfer,
        &format!("prepare, then receive {}", from.display()),
    );
    let args: Vec<String> = worktree::SYNC_ARGS
        .iter()
        .map(|a| a.to_string())
        .chain([format!("{}/", from.display()), format!(":local-{key}/")])
        .collect();
    backend.sync(req, &args, &before, on_report)
}

pub(crate) fn announce_prepared(text: &str) {
    let Ok(prepared) = worktree::parse(text) else {
        return;
    };
    eprintln!("dibs: {}", prepared.worktree);
    if let (Some(from), Some(mine), Some((have, of))) =
        (&prepared.seeded, prepared.reseeded, prepared.seed_shared)
    {
        eprintln!(
            "dibs: this tree's target had built {mine} of the {of} groups in its lockfile and {from} has {have}, so the tree now starts from {from}'s"
        );
        return;
    }
    if let Some(from) = &prepared.seeded {
        match prepared.seed_shared {
            Some((have, of)) => eprintln!(
                "dibs: target directory copied from {from}, whose builds match {have} of the {of} groups in this tree's lockfile{}",
                if prepared.seeded_sources {
                    ", with its sources so unchanged crates stay built"
                } else {
                    ""
                }
            ),
            None => eprintln!(
                "dibs: target directory copied from {from}, so only what differs rebuilds"
            ),
        }
    }
}

/// Adds files and never replaces one: git names objects by their content, so what is already
/// there is already right, and a cargo on the machine may be reading it.
pub(crate) fn sync_gitdb(backend: &Jobs, from: &Path, to: &str) -> Result<(), String> {
    let args = gitdb_args(from, to);
    let req = Request {
        label: "",
        lock: Lock::Shared,
        device: None,
        job: &RecipeJob::default(),
        max: None,
        new_series: false,
    };
    match backend.sync(&req, &args, "", &mut |_| {}).outcome.status {
        0 => Ok(()),
        _ => Err(format!("sending {} failed", from.display())),
    }
}

pub(crate) fn gitdb_args(from: &Path, to: &str) -> [String; 5] {
    [
        "-a".to_string(),
        "--no-times".to_string(),
        "--ignore-existing".to_string(),
        format!("{}/", from.display()),
        format!(":{to}/"),
    ]
}

pub(crate) fn sh(s: &str) -> String {
    ShellWord(s).to_string()
}

/// `dibs raw`: nothing prepared and nothing looked up, the last resort, and recorded so that
/// being a last resort is visible rather than assumed.
pub(crate) fn raw(args: &RecipeCall) -> Result<ExitCode, RunError> {
    let reason = args.reason.as_deref().ok_or(
        "raw needs --reason. It is recorded, and a reason that keeps recurring is what\n             specifies the next recipe. If this fits a recipe, use the recipe instead.",
    )?;
    let command = args.command.as_deref().ok_or("raw needs -- <command>")?;
    // Always shared, and nothing is prepared for it, so it is placed like any other shared work.
    let backend = Jobs::placed(None, None)?;
    let out = backend.run(
        &Request {
            label: "raw",
            lock: Lock::Shared,
            device: args.device.as_deref(),
            job: &RecipeJob::default(),
            max: args.max,
            new_series: args.new_series,
        },
        command,
    );
    let steps = vec![out.step_record(Lock::Shared)];
    write_record(&RunRecord {
        when: now_secs(),
        verb: RunVerb::Raw,
        label: "raw".into(),
        repo: String::new(),
        variant: None,
        recipe: String::new(),
        fingerprint: String::new(),
        isolation: "machine".into(),
        backend: BACKEND.into(),
        machine: backend.machine.clone(),
        device: args.device.as_deref().map(Alias::from),
        needs: None,
        reason: Some(reason.to_string()),
        seeded: None,
        batch: batch_of_caller(),
        refs: None,
        arms: Vec::new(),
        reps: 1,
        anyway: false,
        new_series: false,
        fresh: Pairs::default(),
        state: Pairs::default(),
        params: BTreeMap::new(),
        procedure: vec![ProcedureStep {
            lock: Lock::Shared,
            run: command.to_string(),
        }],
        revisions: Pairs::default(),
        outcome: Some(Outcome::of_steps(&steps)),
        steps,
    })?;
    Ok(ExitCode::from(out.status.clamp(0, 255) as u8))
}
