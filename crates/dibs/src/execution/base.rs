use super::pins;
use super::{
    jobs::{BACKEND, JobOutcome, JobRequest, Jobs, Reported},
    pins::{pin_spec, pins_of},
    record::{batch_of_caller, fetch_artifacts, measured_summary},
    refs::{Arm, arms, sent_from, short, sides},
    schedule::{Job, jobs_of, schedule},
    sweep::{sweep_points, sweep_run},
};
use crate::{
    batch, gitdeps,
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
    StepRecord, wire,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    process::ExitCode,
};

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
    pub(crate) plan: TreePlan,
    pub(crate) local: Option<worktree::Local>,
    pub(crate) prepared: Option<wire::Prepared>,
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
            let backend = Jobs::on(destination(rec, &repo_name, name, &args)?);
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
    let backend = Jobs::on(destination(rec, &repo_name, name, &args)?);
    if let Some(m) = backend.machine.as_ref().filter(|_| !pinned(&args)) {
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
        tree: None,
    };
    let mut announce = |prepared: &wire::Prepared| announce_prepared(prepared);

    // The pinned trees go first: the patch names where they landed.
    let mut pinned = Vec::with_capacity(pins.len());
    for (k, p) in pins.iter_mut().enumerate() {
        let env = env_of(k);
        let setup = JobRequest {
            label: &calls[k].label,
            lock: Lock::Shared,
            device: None,
            job: &env,
            max: None,
            new_series: false,
            tree: None,
        };
        let fresh = Manifest::load_any(&p.dir, &p.repo)?;
        let lock = p.checkout.as_mut().and_then(|c| c.lock.take());
        let from = p
            .checkout
            .as_ref()
            .map_or(p.dir.as_path(), |c| c.dir.as_path());
        let plan = TreeSpec {
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
        .plan();
        let reported = match &p.local {
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
                let send = JobRequest {
                    tree: Some(plan.tree(wire::Then::Transfer)),
                    ..setup
                };
                let reported = sync_prepared(&backend, from, &l.key, &send, &mut announce);
                drop(lock);
                if reported.prepared.is_none() || reported.outcome.status != 0 {
                    return Err(RunError::call(
                        reported.outcome.status,
                        format!(
                            "could not send the pinned {} from {} (exit {})",
                            p.repo,
                            from.display(),
                            reported.outcome.status
                        ),
                    ));
                }
                reported
            }
            None => {
                eprintln!("dibs: pinning {}@{}", p.repo, p.reference);
                let prepare = JobRequest {
                    tree: Some(plan.tree(wire::Then::Nothing)),
                    ..setup
                };
                let title = preparing_title(&p.repo, &p.reference);
                let reported = backend.run_reporting(&prepare, &title, &mut |_| {});
                if reported.outcome.status != 0 {
                    return Err(RunError::call(
                        reported.outcome.status,
                        format!(
                            "could not prepare the pinned {}@{} (exit {})",
                            p.repo, p.reference, reported.outcome.status
                        ),
                    ));
                }
                reported
            }
        };
        let Some(prepared) = reported.prepared else {
            return Err(RunError::call(
                Exit::Setup.status(),
                "the worktree setup did not report a path; see its output above".into(),
            ));
        };
        send_missing_gitdbs(&backend, &prepared, &plan.gitdbs);
        pinned.push(prepared);
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
        let plan = TreeSpec {
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
        .plan();
        slot += usize::from(arm.fetch.is_some());
        trees.push(Tree {
            token,
            plan,
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
        let setup = JobRequest {
            label: &calls[k].label,
            lock: Lock::Shared,
            // Preparing a worktree touches no GPU, so pinning it would only make the setup fail
            // on a machine whose card has been pulled.
            device: None,
            job: &env,
            max: None,
            new_series: false,
            tree: None,
        };
        let (arm, step, rep, fold) = match job {
            Job::Send(a) => {
                let t = &mut trees[a];
                let key = &t.local.as_ref().expect("a sent tree is local").key;
                let send = JobRequest {
                    tree: Some(t.plan.tree(wire::Then::Transfer)),
                    ..setup
                };
                let Reported {
                    outcome: out,
                    prepared,
                    ..
                } = sync_prepared(&backend, arms[a].dir(&dir), key, &send, &mut announce);
                checkout_locks[a] = None;
                let Some(prepared) = prepared else {
                    return Err(RunError::call(
                        out.status,
                        format!("could not prepare {} (exit {})", what(a), out.status),
                    ));
                };
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
                send_missing_gitdbs(&backend, &prepared, &t.plan.gitdbs);
                t.prepared = Some(prepared);
                continue;
            }
            Job::Setup(a) => {
                let t = &mut trees[a];
                let prepare = JobRequest {
                    tree: Some(t.plan.tree(wire::Then::Nothing)),
                    ..setup
                };
                let title = preparing_title(&repo_name, arms[a].fetch.as_deref().unwrap_or(""));
                let Reported {
                    outcome: out,
                    prepared,
                    ..
                } = backend.run_reporting(&prepare, &title, &mut |_| {});
                let Some(prepared) = prepared.filter(|_| out.status == 0) else {
                    return Err(RunError::call(
                        out.status,
                        format!("could not prepare {} (exit {})", what(a), out.status),
                    ));
                };
                announce(&prepared);
                send_missing_gitdbs(&backend, &prepared, &t.plan.gitdbs);
                t.prepared = Some(prepared);
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
        let req = JobRequest {
            label: &step_labels[step],
            lock,
            device: args.device.as_deref(),
            job: &env,
            max: args.max,
            new_series: args.new_series,
            tree: None,
        };
        // One cache per repo, exported rather than left to each recipe to remember. The output
        // needs no file of its own: dibs keeps every job's log under its job id, and a path named
        // after the label would be shared by two runs of one recipe.
        let t = &mut trees[arm];
        let fresh = match args.reps {
            1 => t.token.clone(),
            _ => format!("{}-r{}", t.token, rep.unwrap_or(1)),
        };
        let StepPlan {
            command: run,
            around,
        } = step_plan(rec, step, &t.token, &fresh, args.anyway, patched.as_ref());
        let record = |out: &JobOutcome, stepped: Option<&wire::Stepped>| StepRecord {
            arm: compared.then(|| arms[arm].name.clone()),
            rep: rep.filter(|_| args.reps > 1),
            artifacts: stepped.and_then(|s| s.artifacts),
            ..out.step_record(lock)
        };
        if fold {
            eprintln!(
                "dibs: {}step {}/{} [{lock:?}], with the setup ahead of it",
                tag(arm, rep),
                step + 1,
                rec.steps.len()
            );
            let folded = JobRequest {
                tree: Some(wire::Tree {
                    step: Some(around.clone()),
                    ..t.plan.tree(wire::Then::Step)
                }),
                ..req
            };
            let Reported {
                outcome: out,
                stepped,
                prepared,
            } = backend.run_reporting(&folded, &run, &mut announce);
            let Some(prepared) = prepared else {
                return Err(RunError::call(
                    out.status,
                    format!("could not prepare {} (exit {})", what(arm), out.status),
                ));
            };
            send_missing_gitdbs(&backend, &prepared, &t.plan.gitdbs);
            let waited = held(&prepared);
            t.prepared = Some(prepared);
            if !waited {
                if stepped.as_ref().is_some_and(|s| s.refused) {
                    return Ok(ExitCode::from(Exit::TargetRebuilt.code()));
                }
                if let Some(read) = stepped
                    .as_ref()
                    .and_then(|s| s.state.clone())
                    .filter(|read| !read.is_empty())
                {
                    state = read;
                }
                steps.push(record(&out, stepped.as_ref()));
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
        let there = JobRequest {
            tree: Some(wire::Tree {
                step: Some(around),
                ..in_tree(p)
            }),
            ..req
        };
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
            stepped,
            ..
        } = backend.run_reporting(&there, &run, &mut |_| {});
        // The machine has said why; a refusal is not a run, so it leaves no record.
        if stepped.as_ref().is_some_and(|s| s.refused) {
            return Ok(ExitCode::from(Exit::TargetRebuilt.code()));
        }
        if let Some(read) = stepped
            .as_ref()
            .and_then(|s| s.state.clone())
            .filter(|read| !read.is_empty())
        {
            state = read;
        }
        steps.push(record(&out, stepped.as_ref()));
        if out.status != 0 {
            failed = Some(out.status);
        }
    }
    fetch_artifacts(&backend, &steps, args.artifacts_to.as_deref(), compared);

    let revision = |p: &wire::Prepared| (p.revision.repo.clone(), p.revision.sha.clone());
    let pinned_revisions: Vec<(String, String)> = pinned.iter().map(revision).collect();
    let prepared = |a: usize| trees[a].prepared.as_ref();
    let revisions_of = |a: usize| -> Vec<(String, String)> {
        prepared(a)
            .map(revision)
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
            .and_then(|p| p.seeded.as_ref().map(|s| s.from.clone())),
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
                    seeded: prepared(a).and_then(|p| p.seeded.as_ref().map(|s| s.from.clone())),
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
    args: &RecipeCall,
) -> Result<Option<MachineName>, RunError> {
    if rec.steps.iter().all(|s| s.lock == Lock::Shared) {
        let placed = Jobs::placed(
            args.machine(),
            affinity_get(repo_name).as_deref(),
            Some(repo_name),
        )?;
        return Ok(placed.machine);
    }
    match Jobs::destination(args.machine())? {
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
        let req = JobRequest {
            label: &step_labels[i],
            lock: rec.steps[i].lock,
            device: args.device.as_deref(),
            job: &RecipeJob::default(),
            max: None,
            new_series: args.new_series,
            tree: None,
        };
        if !backend.preflight(&req) {
            return true;
        }
    }
    false
}

/// What step `i` runs in its tree, and what the machine does around it. A build claims the
/// target for this tree, and a measurement after one refuses a target some other tree has built
/// into since, unless told `anyway`. `token` is the tree's prepare, whose package list a build
/// records; `fresh` is this run's. A shared build against pins checks they took.
pub(crate) fn step_plan(
    rec: &recipe::Recipe,
    i: usize,
    token: &str,
    fresh: &str,
    anyway: bool,
    pinned: Option<&BTreeSet<String>>,
) -> StepPlan {
    let step = &rec.steps[i];
    let builds = worktree::build_signature(&step.run).is_some();
    let built = rec.steps[..i]
        .iter()
        .any(|s| s.lock == Lock::Shared && worktree::build_signature(&s.run).is_some());
    let measured = step.lock == Lock::Exclusive;
    // Exported rather than prefixed onto the command, so it reaches a pipeline or a loop in the
    // step as well as the first word of it.
    let exports: String = fresh_values(rec, fresh)
        .iter()
        .chain(&step.env)
        .map(|(k, v)| format!("export {k}={}; ", sh(v)))
        .collect();
    StepPlan {
        command: format!("{exports}{}", step.run),
        around: wire::Step {
            claim: builds && step.lock == Lock::Shared,
            record: builds.then(|| token.to_string()),
            check: measured && built && !anyway,
            state: measured,
            artifacts: rec.artifacts.clone(),
            pinned: match pinned {
                Some(names) if builds && step.lock == Lock::Shared => {
                    names.iter().cloned().collect()
                }
                _ => Vec::new(),
            },
        },
    }
}

/// A step's command, and what the machine does around it.
#[derive(Debug, PartialEq)]
pub(crate) struct StepPlan {
    pub(crate) command: String,
    pub(crate) around: wire::Step,
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

/// A tree as the machine is asked to lay it out, and the git databases it may need sent.
pub(crate) struct TreePlan {
    pub(crate) prepare: wire::Prepare,
    pub(crate) gitdbs: Vec<gitdeps::Db>,
}

impl TreeSpec<'_> {
    pub(crate) fn plan(&self) -> TreePlan {
        let lock = lockfile(self.dir, self.local.is_none().then_some(self.reference));
        let lock = lock.as_deref().unwrap_or("");
        let gitdbs = gitdeps::local(&gitdeps::cargo_home(), &gitdeps::pinned(lock));
        let lines = worktree::packages(lock, self.signature);
        let prepare = wire::Prepare {
            repo: self.repo_name.to_string(),
            source: match self.local {
                Some(l) => wire::Source::Local {
                    key: l.key.clone(),
                    content: l.content.clone(),
                },
                None => wire::Source::Fetched {
                    reference: self.reference.to_string(),
                    slot: self.slot as u32,
                },
            },
            nest: self.nest.map(|n| wire::Nest {
                name: n.name.clone(),
                config: n.config.clone(),
            }),
            fresh: self.fresh.to_vec(),
            packages: (!lines.is_empty()).then(|| wire::Packages {
                token: self.token.to_string(),
                lines,
            }),
            gitdbs: gitdbs
                .iter()
                .map(|db| wire::GitDb {
                    name: db.name.clone(),
                    commit: db.commit.clone(),
                })
                .collect(),
        };
        TreePlan { prepare, gitdbs }
    }
}

impl TreePlan {
    /// Laid out at the head of a job, which then does `then`.
    pub(crate) fn tree(&self, then: wire::Then) -> wire::Tree {
        wire::Tree {
            place: wire::Place::Prepare(self.prepare.clone()),
            then,
            step: None,
        }
    }
}

/// A step in a tree laid out before.
pub(crate) fn in_tree(prepared: &wire::Prepared) -> wire::Tree {
    wire::Tree {
        place: wire::Place::At(wire::At {
            worktree: prepared.worktree.clone(),
            target: prepared.target.clone(),
        }),
        then: wire::Then::Step,
        step: None,
    }
}

/// What a job that only lays out a tree runs, which `--status` and `--log` show of it.
pub(crate) fn preparing_title(repo: &str, reference: &str) -> String {
    format!("# prepare {repo}@{reference}")
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

pub(crate) fn send_missing_gitdbs(
    backend: &Jobs,
    prepared: &wire::Prepared,
    gitdbs: &[gitdeps::Db],
) {
    let Some(asked) = &prepared.gitdbs else {
        return;
    };
    for missing in &asked.missing {
        let Some(db) = gitdbs
            .iter()
            .find(|d| d.name == missing.name && d.commit == missing.commit)
        else {
            continue;
        };
        eprintln!(
            "dibs: sending {} at {:.8}, which the machine does not have and may not be able to fetch",
            db.name, db.commit
        );
        if let Err(e) = sync_gitdb(backend, &db.path, &format!("{}/{}", asked.dir, db.name)) {
            eprintln!("dibs: {e}; the build will try to fetch it itself");
        }
    }
}

/// Whether a step's tree waits for a git dependency to be sent before it can build.
pub(crate) fn held(prepared: &wire::Prepared) -> bool {
    prepared
        .gitdbs
        .as_ref()
        .is_some_and(|g| !g.missing.is_empty())
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
    key: &str,
    req: &JobRequest,
    on_prepared: &mut dyn FnMut(&wire::Prepared),
) -> Reported {
    let args: Vec<String> = worktree::SYNC_ARGS
        .iter()
        .map(|a| a.to_string())
        .chain([format!("{}/", from.display()), format!(":local-{key}/")])
        .collect();
    backend.sync(req, &args, on_prepared)
}

pub(crate) fn announce_prepared(prepared: &wire::Prepared) {
    eprintln!("dibs: {}", prepared.worktree);
    let Some(seeded) = &prepared.seeded else {
        return;
    };
    let from = &seeded.from;
    if let (Some(mine), Some(shared)) = (prepared.reseeded, seeded.shared) {
        eprintln!(
            "dibs: this tree's target had built {mine} of the {} groups in its lockfile and {from} has {}, so the tree now starts from {from}'s",
            shared.of, shared.have
        );
        return;
    }
    match seeded.shared {
        Some(shared) => eprintln!(
            "dibs: target directory copied from {from}, whose builds match {} of the {} groups in this tree's lockfile{}",
            shared.have,
            shared.of,
            if seeded.sources {
                ", with its sources so unchanged crates stay built"
            } else {
                ""
            }
        ),
        None => {
            eprintln!("dibs: target directory copied from {from}, so only what differs rebuilds")
        }
    }
}

/// Adds files and never replaces one: git names objects by their content, so what is already
/// there is already right, and a cargo on the machine may be reading it.
pub(crate) fn sync_gitdb(backend: &Jobs, from: &Path, to: &str) -> Result<(), String> {
    let args = gitdb_args(from, to);
    let req = JobRequest {
        label: "",
        lock: Lock::Shared,
        device: None,
        job: &RecipeJob::default(),
        max: None,
        new_series: false,
        tree: None,
    };
    match backend.sync(&req, &args, &mut |_| {}).outcome.status {
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
    let backend = Jobs::placed(args.machine(), None, None)?;
    let out = backend.run(
        &JobRequest {
            label: "raw",
            lock: Lock::Shared,
            device: args.device.as_deref(),
            job: &RecipeJob::default(),
            max: args.max,
            new_series: args.new_series,
            tree: None,
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
