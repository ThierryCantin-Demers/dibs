use super::{
    error::{Refusal, RunError, Unprepared},
    jobs::{BACKEND, JobOutcome, JobRequest, Jobs, Reported},
    local::Repo,
    pins::{self, pin_spec, pins_of},
    record::{batch_of_caller, fetch_artifacts, measured_summary},
    refs::{Arm, Side},
    schedule::{Job, jobs_of, schedule},
    sweep::{sweep_points, sweep_run},
    trees::{
        TreePlan, TreeSpec, announce_prepared, held, in_tree, new_token, preparing_title,
        send_missing_gitdbs, sync_prepared,
    },
};
use crate::{
    batch,
    call::{Destination, RecipeJob},
    cli::{RecipeCall, ShellWord},
    recipe::{self, Lock, Manifest, NotTaken, Resolved},
    records::{Affinity, RunLog, now_secs},
};
use dibs_format::{
    Alias, ArmRecord, Exit, MachineName, Outcome, Pairs, ProcedureStep, RunRecord, RunVerb,
    StepRecord, wire,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::ExitCode,
};

/// What is about to be prepared, for the person reading along.
pub fn preparing(repo: &str, arm: &Arm, local: Option<&super::Local>, dir: &Path) -> String {
    match (&arm.checkout, local) {
        (Some(c), _) => format!(
            "{repo} at {}{}",
            c.short_sha(),
            c.sent_from(arm.note.as_deref(), "this computer")
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
pub struct Tree {
    pub token: String,
    pub plan: TreePlan,
    pub local: Option<super::Local>,
    pub prepared: Option<wire::Prepared>,
}

pub fn run_recipe(args: RecipeCall) -> Result<ExitCode, RunError> {
    let points = sweep_points(&args);
    if points.len() > 1 {
        return sweep_run(&args, &points);
    }
    // One point is the ordinary call with its values filled in, not a batch of one.
    let args = RecipeCall {
        params: points.into_iter().next().unwrap_or_default(),
        ..args
    };

    let sides = Side::list(args.reference.as_deref())?;
    let resolved = Resolved::of(&args)?;
    let mut arms = Arm::look_up(&sides, &resolved.dir, &resolved.repo_name)?;
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
                    c.sent_from(arm.note.as_deref(), &c.dir.display().to_string())
                ),
                (None, None) => {
                    let l = super::Local::of(&dir)?;
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
                    c.sent_from(p.note.as_deref(), &c.dir.display().to_string())
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
    if let Some(m) = backend.machine.as_ref().filter(|_| !args.pinned()) {
        Affinity::here().set(&repo_name, m.as_str());
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
                        c.short_sha(),
                        c.sent_from(p.note.as_deref(), "this computer")
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
                    return Err(RunError::Call {
                        exit: reported.outcome.status,
                        failed: Unprepared::SendPinned {
                            repo: p.repo.clone(),
                            from: from.to_path_buf(),
                        },
                    });
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
                    return Err(RunError::Call {
                        exit: reported.outcome.status,
                        failed: Unprepared::PreparePinned {
                            repo: p.repo.clone(),
                            reference: p.reference.clone(),
                        },
                    });
                }
                reported
            }
        };
        let Some(prepared) = reported.prepared else {
            return Err(RunError::Call {
                exit: Exit::Setup.status(),
                failed: Unprepared::NoPath,
            });
        };
        send_missing_gitdbs(&backend, &prepared, &plan.gitdbs);
        pinned.push(prepared);
    }
    let nest = (!pins.is_empty()).then(|| {
        super::Nest::new(pins::config(
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
        .find_map(|st| super::build_signature(&st.run))
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
                    return Err(RunError::Call {
                        exit: out.status,
                        failed: Unprepared::Prepare(what(a)),
                    });
                };
                if out.status != 0 {
                    return Err(RunError::Call {
                        exit: out.status,
                        failed: Unprepared::Send(arms[a].dir(&dir).to_path_buf()),
                    });
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
                    return Err(RunError::Call {
                        exit: out.status,
                        failed: Unprepared::Prepare(what(a)),
                    });
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
                return Err(RunError::Call {
                    exit: out.status,
                    failed: Unprepared::Prepare(what(arm)),
                });
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
        variant: Repo(&dir).variant(&repo_name),
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
    RunLog::here()?.append(&record)?;

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
pub fn destination(
    rec: &recipe::Recipe,
    repo_name: &str,
    name: &str,
    args: &RecipeCall,
) -> Result<Option<MachineName>, RunError> {
    if rec.steps.iter().all(|s| s.lock == Lock::Shared) {
        let placed = Jobs::placed(
            args.machine(),
            Affinity::here().get(repo_name).as_deref(),
            Some(repo_name),
        )?;
        return Ok(placed.machine);
    }
    match Jobs::destination(args.machine())? {
        Destination::Named(m) => Ok(Some(m)),
        Destination::Unnamed => Ok(None),
        Destination::Unchosen => Err(Refusal::Unmeasured(name.to_string()).into()),
    }
}

/// Whether the wrapper refuses a measured step, or the first step when a card is named, on what it
/// decides without the machine. A shared recipe's card is otherwise checked by the first step that
/// carries it, after the tree and its dependencies have been sent.
fn refused_before_building(
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
pub fn step_plan(
    rec: &recipe::Recipe,
    i: usize,
    token: &str,
    fresh: &str,
    anyway: bool,
    pinned: Option<&BTreeSet<String>>,
) -> StepPlan {
    let step = &rec.steps[i];
    let builds = super::build_signature(&step.run).is_some();
    let built = rec.steps[..i]
        .iter()
        .any(|s| s.lock == Lock::Shared && super::build_signature(&s.run).is_some());
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
pub struct StepPlan {
    pub command: String,
    pub around: wire::Step,
}

/// One value per run for each of the recipe's `fresh` variables, the same in every step of it.
fn fresh_values(rec: &recipe::Recipe, token: &str) -> BTreeMap<String, String> {
    rec.fresh
        .iter()
        .map(|v| (v.clone(), format!("dibs-{token}")))
        .collect()
}

pub fn sh(s: &str) -> String {
    ShellWord(s).to_string()
}

/// `dibs raw`: nothing prepared and nothing looked up, the last resort, and recorded so that
/// being a last resort is visible rather than assumed.
pub fn raw(args: &RecipeCall) -> Result<ExitCode, RunError> {
    let reason = args.reason.as_deref().ok_or(Refusal::RawReason)?;
    let command = args.command.as_deref().ok_or(Refusal::RawCommand)?;
    let not_taken = NotTaken::of(args.params.keys());
    if !not_taken.is_empty() {
        return Err(Refusal::RawNotTaken(not_taken).into());
    }
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
    RunLog::here()?.append(&RunRecord {
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
