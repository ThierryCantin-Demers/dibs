use super::{
    base::{
        RunError, TreeSpec, announce_prepared, new_token, preparing, preparing_title,
        send_missing_gitdbs, sh, sync_prepared,
    },
    jobs::{JobRequest, Jobs},
    refs::{arms, sides},
};
use crate::{
    recipe::{self, Lock, Manifest, resolve_repo, root_of, run_label},
    records::{affinity_set, pinned},
    worktree,
};
use dibs::{
    call::{Destination, Dispatch, RecipeJob},
    caller::Caller,
    cli::{
        Call, CliError, Command as ShellCommand, Mode, PortName, RecipeCall, Run, RunLock, Service,
    },
};
use dibs_format::{Alias, Exit, Label, MachineName, wire};
use std::process::ExitCode;

/// A repo's servers, running on the machine under one lock while the command runs here: a
/// dashboard, a client, a test suite driving them over the network. It ends by becoming that
/// dibs call rather than waiting on one, so the command keeps this terminal.
pub(crate) fn with_service(args: &RecipeCall) -> Result<ExitCode, RunError> {
    let sides = sides(args.reference.as_deref())?;
    if sides.len() > 1 {
        return Err("with runs against one tree, so it takes one ref".into());
    }
    if !args.pins.is_empty() {
        return Err("with does not take --pin; a recipe does".into());
    }
    let dir = resolve_repo(&args.repo, &root_of(args)?)?;
    let repo_name = worktree::identity(&dir);
    let manifest = Manifest::load(&dir, &repo_name)?;
    let name = args.recipe.as_deref().ok_or_else(|| {
        format!(
            "with needs a service: dibs with {} <service> -- <command>",
            args.repo
        )
    })?;
    let svc = manifest.service(name).ok_or_else(|| {
        let have: Vec<&str> = manifest.service_listing().iter().map(|(n, _)| *n).collect();
        match have.is_empty() {
            true => format!("{repo_name} defines no services, so there is nothing to run against"),
            false => format!(
                "no service '{name}' for {repo_name}. It has: {}",
                have.join(", ")
            ),
        }
    })?;
    if svc.serves.is_empty() {
        return Err(format!(
            "service '{name}' starts nothing: it needs a [[service.{name}.serve]] with a run"
        )
        .into());
    }
    let command = args
        .command
        .as_deref()
        .ok_or("with needs a command after --")?;

    let backend = Jobs::on(match Jobs::destination(args.machine())? {
        Destination::Named(m) => Some(m),
        Destination::Unnamed => None,
        Destination::Unchosen => {
            return Err("with starts servers on a machine this computer then drives, so it names one: --on <machine>,\n  \
                        or export DIBS_ON. dibs --machines lists them."
                .into())
        }
    });
    let label = run_label(&repo_name, "with", Some(name), args.device.as_deref());
    if args.device.is_some() {
        let req = JobRequest {
            label: &label,
            lock: Lock::Shared,
            device: args.device.as_deref(),
            job: &RecipeJob::default(),
            max: None,
            new_series: false,
            tree: None,
        };
        if !backend.preflight(&req) {
            return Ok(ExitCode::from(Exit::Refused.code()));
        }
    }

    let mut arm = arms(&sides, &dir, &repo_name)?.remove(0);
    let local = arm.fetch.is_none().then(|| arm.local(&dir)).transpose()?;
    if args.dry_run {
        println!("label       {label}");
        println!(
            "machine     {}",
            backend
                .machine
                .as_ref()
                .map_or("the only one", MachineName::as_str)
        );
        println!(
            "tree        {}",
            preparing(&repo_name, &arm, local.as_ref(), &dir)
        );
        if let Some(d) = &args.device {
            println!("device      {d}");
        }
        if let Some(b) = &svc.build {
            println!("build       [Shared] {b}");
        }
        for serve in &svc.serves {
            println!("serve       {}: {}", serve.name, serve.run);
            if let Some(r) = &serve.ready {
                println!("            ready {r}");
            }
        }
        for p in &svc.ports {
            println!("port        {p}, as $DIBS_PORT_{}", p.to_uppercase());
        }
        println!(
            "command     [{}] {command}, {}",
            if args.bench { "Exclusive" } else { "Shared" },
            if args.there {
                "on the machine, in the tree"
            } else {
                "here"
            }
        );
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(m) = backend.machine.as_ref().filter(|_| !pinned(args)) {
        affinity_set(&repo_name, m.as_str());
    }
    eprintln!(
        "dibs: preparing {}",
        preparing(&repo_name, &arm, local.as_ref(), &dir)
    );
    let reference = arm.fetch.as_deref().unwrap_or("local");
    let from = arm.dir(&dir).to_path_buf();
    let signature = svc
        .build
        .as_deref()
        .and_then(worktree::build_signature)
        .unwrap_or_default();
    let plan = TreeSpec {
        dir: &from,
        repo_name: &repo_name,
        reference,
        local: local.as_ref(),
        signature: &signature,
        token: &new_token(),
        slot: 0,
        nest: None,
        fresh: manifest.tree_fresh(),
    }
    .plan();
    let setup_label = format!("{label}:{}", if local.is_some() { "send" } else { "setup" });
    let setup = JobRequest {
        label: &setup_label,
        lock: Lock::Shared,
        device: None,
        job: &RecipeJob::default(),
        max: None,
        new_series: false,
        tree: None,
    };
    let reported = match &local {
        Some(l) => {
            let send = JobRequest {
                tree: Some(plan.tree(wire::Then::Transfer)),
                ..setup
            };
            let reported = sync_prepared(&backend, &from, &l.key, &send, &mut announce_prepared);
            if let Some(c) = &mut arm.checkout {
                c.lock = None;
            }
            if reported.prepared.is_none() || reported.outcome.status != 0 {
                return Err(RunError::call(
                    reported.outcome.status,
                    format!(
                        "could not prepare {repo_name} from {} (exit {})",
                        from.display(),
                        reported.outcome.status
                    ),
                ));
            }
            reported
        }
        None => {
            let prepare = JobRequest {
                tree: Some(plan.tree(wire::Then::Nothing)),
                ..setup
            };
            let title = preparing_title(&repo_name, reference);
            let reported = backend.run_reporting(&prepare, &title, &mut |_| {});
            if reported.outcome.status != 0 {
                return Err(RunError::call(
                    reported.outcome.status,
                    format!(
                        "could not prepare {repo_name}@{reference} (exit {})",
                        reported.outcome.status
                    ),
                ));
            }
            if let Some(prepared) = &reported.prepared {
                announce_prepared(prepared);
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

    // In the tree, with the repo's build cache, exactly as a recipe step runs.
    let in_tree = |run: &str| {
        format!(
            "cd {} && export CARGO_TARGET_DIR={} && {{ {run}; }}",
            sh(&prepared.worktree),
            sh(&prepared.target)
        )
    };
    if let Some(build) = &svc.build {
        eprintln!("dibs: building {name}");
        let build_label = format!("{label}:build");
        let req = JobRequest {
            label: &build_label,
            lock: Lock::Shared,
            device: args.device.as_deref(),
            job: &RecipeJob::default(),
            max: None,
            new_series: false,
            tree: None,
        };
        let build = match worktree::build_signature(build) {
            Some(_) => worktree::claiming(build),
            None => build.clone(),
        };
        let out = backend.run(&req, &in_tree(&build));
        if out.status != 0 {
            return Ok(ExitCode::from(out.status.clamp(1, 255) as u8));
        }
    }

    // Timed against a server this call built, a server another tree has built over since is refused.
    let guarded = args.bench
        && !args.anyway
        && svc
            .build
            .as_deref()
            .and_then(worktree::build_signature)
            .is_some();
    let run = match served(svc, args, command, guarded, &in_tree) {
        Ok(run) => run,
        Err(e) => {
            eprintln!("{e}");
            return Ok(ExitCode::from(e.exit()));
        }
    };
    let call = Call {
        mode: Mode::Run(run),
        on: backend.machine.clone(),
        label: Some(Label::new(&label)),
        max: args.max,
        device: args.device.as_deref().map(Alias::new),
        ..Call::default()
    };
    let code = Dispatch {
        call: &call,
        caller: &Caller::from_env(),
    }
    .exit();
    Ok(ExitCode::from(code.rem_euclid(256) as u8))
}

/// The call `with` ends in: its servers, its ports and the command, held here unless `--there`.
pub(crate) fn served(
    svc: &recipe::Service,
    args: &RecipeCall,
    command: &str,
    guarded: bool,
    in_tree: &dyn Fn(&str) -> String,
) -> Result<Run, CliError> {
    let mut ports = Vec::new();
    for p in &svc.ports {
        ports.push(PortName::declared(p, &ports)?);
    }
    let mut services = Vec::new();
    for serve in &svc.serves {
        let run = match guarded {
            true => worktree::checked(&serve.run),
            false => serve.run.clone(),
        };
        services.push(Service::declared(
            &serve.name,
            &in_tree(&run),
            serve.ready.as_deref(),
            &services,
        )?);
    }
    let run = Run {
        lock: match args.bench {
            true => RunLock::Bench,
            false => RunLock::Shared,
        },
        hold: !args.there,
        services,
        ports,
        command: ShellCommand(vec![match args.there {
            true => in_tree(command),
            false => command.to_string(),
        }]),
        ..Run::default()
    };
    run.refuse_unknown_ports()?;
    Ok(run)
}
