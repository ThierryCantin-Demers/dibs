//! The agent-facing half of dibs: verbs, recipes, labels and provenance, over a resource layer
//! whose only job is to hand back a machine with the right things held.
//!
//! It runs here rather than on the target, which is why it can be a program rather than a
//! shell script. The half that ships over ssh stays bash on purpose: installing nothing on a
//! machine is what makes adding one cheap.
//!
//! The verbs exist because an interface taking one arbitrary string invites the four problems
//! measured in the log it replaces. Labels were unstable, so estimates could not work. Two
//! jobs in 179 redirected their output, so watching one almost never worked. Agents chose
//! their own scratch paths, and one filled a shared quota. And the rule to build under the
//! shared lock was prose, so 17% of all exclusive time was spent compiling.

mod batch;
mod gitdeps;
mod provenance;
mod recipe;
mod resource;
mod runs;
mod worktree;

use recipe::{Lock, Manifest, Verb};
use resource::{Backend, Dibs, Request};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
dibs <verb> <repo>[@<ref>] <recipe>       run a recipe from the repo's .dibs.toml
                                          @local sends your working tree, unpushed and
                                          uncommitted changes included, and is the only
                                          path for a private repo: the machines carry no
                                          GitHub credentials
dibs list <repo>                      what that repo defines
dibs runs [label]                     what has run here, and what is comparable
dibs shell <repo>[@<ref>] --reason <why> [--bench] -- <cmd>   a command in a prepared worktree
dibs raw --reason <why> -- <cmd>      a command with nothing prepared
dibs with <repo>[@<ref>] <service> -- <cmd>   run the command here while the repo's servers
                                      run on the machine under its lock, started once they
                                      are ready and stopped when the command ends
dibs gaps                             what did not fit a recipe, and what recurs
dibs batch <file|->                   a list of dibs command lines as one submission, with one
                                      summary at the end. One line per step, optionally
                                      prefixed [name after=a,b cont]. A step without after=
                                      waits for the one before it; steps that wait for nothing
                                      in common overlap only on different machines. A failed
                                      step stops the batch unless it is marked cont.

  <verb>    bench, build or test
  <repo>    a path to a checkout, or a name resolved under --root
  --root    where named repos live (default $DIBS_ROOT, then `root` in machines.toml,
            else the current directory)
  --reason  why this does not fit a recipe. Required for shell and raw, and recorded:
            a reason that keeps recurring is the specification for the next recipe.
  --device  the card to run on, named from the machine's inventory. It is part of the
            derived label, so each card keeps its own history and running a recipe on a
            second one neither mixes with the first nor replaces it. `dibs --machines -v`
            lists the aliases.
  --<name>  a value for a parameter the recipe declares, such as --backend vulkan.
            `dibs list <repo>` prints what each recipe takes, with its default and its
            choices. The label does not carry them, so one recipe keeps one duration
            history; the run record carries them.
  --sweep   <name>=<a,b,c>, one run per value, submitted as one batch with one summary.
            Repeatable, and the combinations are the cross product. A value is never split
            on commas, so --sweep is how a sweep is asked for and --<name> always means
            one value.
  --reps    run each point this many times, in one batch
  --bench   shell only: the exclusive lock, for a one-off that is a measurement
  --max     seconds the job may hold the lock, when the default is too short for it
  --anyway  measure even when another tree built into the target after this one did,
            which is otherwise refused with exit 78
  --dry-run print what would run, take no lock, record nothing
  --verbose with batch, each step's output as it comes, prefixed with the step's name

A recipe declares the procedure and names no revisions: the invocation supplies the code and
the run record captures what it resolved to.

Which machine comes from DIBS_HOST, the same as it does for dibs itself.
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("dibs: {e}");
            ExitCode::from(2)
        }
    }
}

#[derive(Clone)]
struct Args {
    verb: String,
    repo: String,
    reference: Option<String>,
    recipe: Option<String>,
    root: PathBuf,
    dry_run: bool,
    reason: Option<String>,
    /// Everything after `--`, unsplit. A command is one string here because it is one string
    /// on the far side, and taking it apart only to put it back would change it.
    command: Option<String>,
    /// The card to run on, named from the machine's inventory.
    device: Option<String>,
    /// `--<name> <value>` for whatever the recipe declares. Unknown here rather than refused,
    /// because which names are valid is the recipe's to say, and it says so with the list.
    params: BTreeMap<String, String>,
    /// `--sweep <name>=<a,b,c>`, in the order given, so the points come out in an order a
    /// reader can follow. Spelled apart from `--<name>` because a value may contain a comma:
    /// splitting one would make `--problems a,b` mean two runs of one problem each.
    sweep: Vec<(String, Vec<String>)>,
    reps: u32,
    /// shell only: the exclusive lock, for a one-off that is a measurement.
    bench: bool,
    max: Option<u64>,
    /// Measure even when another tree built into the target after this one did.
    anyway: bool,
    verbose: bool,
}

fn parse() -> Result<Args, String> {
    parse_words(std::env::args().skip(1))
}

fn parse_words(words: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut positional: Vec<String> = Vec::new();
    let mut root = repo_root();
    let mut dry_run = false;
    let mut reason = None;
    let mut command = None;
    let mut device: Option<String> = None;
    let mut params: BTreeMap<String, String> = BTreeMap::new();
    let mut sweep: Vec<(String, Vec<String>)> = Vec::new();
    let mut reps: u32 = 1;
    let mut bench = false;
    let mut max = None;
    let mut anyway = false;
    let mut verbose = false;
    let mut it = words.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => {
                let rest: Vec<String> = it.by_ref().collect();
                if rest.is_empty() {
                    return Err("-- needs a command after it".into());
                }
                command = Some(rest.join(" "));
                break;
            }
            "--reason" => reason = Some(it.next().ok_or("--reason needs a sentence")?),
            "--device" => device = Some(it.next().ok_or("--device needs an alias")?),
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--version" => {
                // Stamped by install.sh, so a binary that has drifted from the source can be
                // told apart from one that is current.
                println!(
                    "dibs-core {} ({})",
                    env!("CARGO_PKG_VERSION"),
                    option_env!("DIBS_CORE_COMMIT").unwrap_or("commit unknown")
                );
                std::process::exit(0);
            }
            "--root" => root = PathBuf::from(it.next().ok_or("--root needs a path")?),
            "--sweep" => {
                let s = it.next().ok_or("--sweep needs <name>=<value,value,...>")?;
                let (name, values) = s.split_once('=').ok_or_else(|| {
                    format!("--sweep takes <name>=<value,value,...>, not {s}")
                })?;
                sweep.push((name.to_string(), values.split(',').map(str::to_string).collect()));
            }
            "--reps" => {
                reps = it
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|n| *n > 0)
                    .ok_or("--reps needs a count")?;
            }
            "--bench" | "-b" => bench = true,
            "--max" => {
                max = Some(it.next().and_then(|n| n.parse().ok()).ok_or("--max needs seconds")?);
            }
            "--anyway" => anyway = true,
            "--dry-run" => dry_run = true,
            "--verbose" | "-v" => verbose = true,
            "-" => positional.push("-".into()),
            s if s.starts_with("--") => {
                let (name, value) = match s.split_once('=') {
                    Some((n, v)) => (n, Some(v.to_string())),
                    None => (s, None),
                };
                let name = name.trim_start_matches('-').to_string();
                let value = match value {
                    Some(v) => v,
                    None => it
                        .next()
                        .filter(|v| !v.starts_with("--"))
                        .ok_or(format!("--{name} needs a value, or is not a flag dibs has"))?,
                };
                params.insert(name, value);
            }
            s if s.starts_with('-') => return Err(format!("unknown option: {s}")),
            s => positional.push(s.to_string()),
        }
    }
    if positional.is_empty() {
        print!("{USAGE}");
        std::process::exit(2);
    }
    let verb = positional.remove(0);
    let target = positional.first().cloned().unwrap_or_default();
    if target.is_empty() && !matches!(verb.as_str(), "runs" | "gaps" | "raw") {
        return Err("needs a repo".into());
    }
    if verb == "with" && command.is_none() {
        return Err("with runs a command here against the repo's servers: dibs with <repo>[@<ref>] <service> -- <command>".into());
    }
    // runs takes a recorded label, not repo@ref, and a label carries its device after an @.
    // Splitting there drops the half that tells two runs of one recipe on different cards apart.
    let (repo, reference) = match target.split_once('@') {
        Some((r, rev)) if verb != "runs" && verb != "batch" => (r.to_string(), Some(rev.to_string())),
        _ => (target, None),
    };
    Ok(Args {
        verb,
        repo,
        reference,
        recipe: positional.get(1).cloned(),
        root,
        dry_run,
        reason,
        command,
        device,
        params,
        sweep,
        reps,
        bench,
        max,
        anyway,
        verbose,
    })
}

/// Where a bare repo name is looked up. Everyone lays their checkouts out differently, so
/// this is only a starting guess: DIBS_ROOT, then --root, then the directory you are in.
fn repo_root() -> PathBuf {
    if let Some(r) = std::env::var_os("DIBS_ROOT").filter(|r| !r.is_empty()) {
        return PathBuf::from(r);
    }
    // A fresh non-interactive shell has no DIBS_ROOT, since it lives in the user's fish
    // config, so the inventory file may carry it: `root = "/home/me/prog"` at the top level.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let inv = std::env::var_os("DIBS_MACHINES").map(PathBuf::from).or_else(|| {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")))
            .map(|c| c.join("dibs/machines.toml"))
    });
    if let Some(text) = inv.and_then(|p| std::fs::read_to_string(p).ok()) {
        for line in text.lines().take_while(|l| !l.trim_start().starts_with('[')) {
            if let Some(v) = line.trim().strip_prefix("root") {
                let v = v.trim_start();
                if let Some(v) = v.strip_prefix('=') {
                    let v = v.trim().trim_matches('"');
                    let v = match (v.strip_prefix("~/"), &home) {
                        (Some(rest), Some(h)) => h.join(rest),
                        _ => PathBuf::from(v),
                    };
                    return v;
                }
            }
        }
    }
    PathBuf::from(".")
}

fn run() -> Result<ExitCode, String> {
    let args = parse()?;

    if args.verb == "batch" {
        let text = match args.repo.as_str() {
            "-" => std::io::read_to_string(std::io::stdin()).map_err(|e| format!("reading the batch from stdin: {e}"))?,
            path => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
        };
        let code = batch::run(&text, &batch::Options { dry_run: args.dry_run, verbose: args.verbose })?;
        return Ok(ExitCode::from(code.clamp(0, 255) as u8));
    }

    if args.verb == "gaps" {
        print!("{}", runs::gaps(&runs::load(&runs_path()?)?));
        return Ok(ExitCode::SUCCESS);
    }

    // Nothing prepared, nothing looked up: the last resort, and instrumented so that being a
    // last resort is visible rather than assumed.
    if args.verb == "raw" {
        let reason = args.reason.as_deref().ok_or(
            "raw needs --reason. It is recorded, and a reason that keeps recurring is what\n             specifies the next recipe. If this fits a recipe, use the recipe instead.",
        )?;
        let command = args.command.as_deref().ok_or("raw needs -- <command>")?;
        // Always shared, and nothing is prepared for it, so there is no reason it should not
        // be ranked like any other shared work.
        let mut backend = Dibs::default();
        if std::env::var("DIBS_ROUTE").as_deref() == Ok("1") {
            backend.machine = Dibs::routed(&backend.program, None, None);
        }
        let out = backend.run(
            &Request {
                label: "raw",
                lock: Lock::Shared,
                isolation: recipe::Isolation::Machine,
                needs: None,
                device: args.device.as_deref(),
                env: &[],
                max: args.max,
            },
            command,
        )?;
        write_record(&provenance::Run {
            label: "raw".into(),
            verb: "raw",
            recipe: String::new(),
            fingerprint: String::new(),
            isolation: "machine".into(),
            needs: None,
            reason: Some(reason.to_string()),
            procedure: vec![("shared".into(), command.to_string())],
            params: BTreeMap::new(),
            backend: backend.name(),
            device: args.device.clone(),
            machine: backend.machine.clone(),
            revisions: Vec::new(),
            seeded: None,
            steps: vec![provenance::StepRecord {
                lock: "shared",
                status: out.status,
                seconds: out.seconds,
            }],
        })?;
        return Ok(ExitCode::from(out.status.clamp(0, 255) as u8));
    }

    // Reads only what this machine recorded, so it needs no repo and no connection.
    if args.verb == "runs" {
        let label = if args.repo.is_empty() { None } else { Some(args.repo.as_str()) };
        let records = runs::load(&runs_path()?)?;
        print!("{}", runs::report(&records, label, 30));
        return Ok(ExitCode::SUCCESS);
    }

    if args.verb == "with" {
        return with_service(&args);
    }

    if args.verb == "list" {
        let dir = resolve_repo(&args.repo, &args.root)?;
        let manifest = Manifest::load(&dir, &worktree::identity(&dir))?;
        for v in [Verb::Bench, Verb::Build, Verb::Test] {
            let listing = manifest.listing(v);
            if !listing.is_empty() {
                println!("{}:", v.as_str());
                for (n, src) in listing {
                    match src {
                        recipe::Source::Builtin => println!("  {n}"),
                        recipe::Source::Repo => println!("  {n}   (from the repo)"),
                        recipe::Source::Local => println!("  {n}   (your local config)"),
                    }
                    // What it accepts, so the valid invocations can be read off rather than
                    // reconstructed from the recipe file.
                    for (p, spec) in manifest.recipe(v, n).map(|r| &r.params).into_iter().flatten() {
                        let choices = match spec.choices.is_empty() {
                            true => String::new(),
                            false => format!("  one of {}", spec.choices.join(", ")),
                        };
                        match &spec.default {
                            Some(d) => println!("      --{p} {d}{choices}"),
                            None => println!("      --{p} <value>, required{choices}"),
                        }
                    }
                }
            }
        }
        let services = manifest.service_listing();
        if !services.is_empty() {
            println!("service:");
            for (n, src) in services {
                match src {
                    recipe::Source::Builtin => println!("  {n}"),
                    recipe::Source::Repo => println!("  {n}   (from the repo)"),
                    recipe::Source::Local => println!("  {n}   (your local config)"),
                }
            }
        }
        println!("\nlocal recipes: {}", recipe::local_dir().display());
        return Ok(ExitCode::SUCCESS);
    }

    let points = sweep_points(&args);
    if points.len() as u32 * args.reps > 1 {
        return sweep_run(&args, &points);
    }
    // One point is the ordinary call with its values filled in, not a batch of one.
    let args = Args { params: points.into_iter().next().unwrap_or_default(), ..args };

    let resolved = resolve(&args)?;
    let calls = jobs_of(&resolved, args.reference.as_deref() == Some("local"));
    let Resolved { dir, repo_name, verb, name, rec, label, step_labels, shell_reason, params } = resolved;
    let (name, rec) = (name.as_str(), &rec);
    let fingerprint = rec.fingerprint();

    if args.dry_run {
        println!("label       {label}");
        println!("recipe      {name}  ({fingerprint})");
        println!("isolation   {:?}", rec.isolation);
        if let Some(n) = &rec.needs {
            println!("needs       {n}");
        }
        match args.reference.as_deref() {
            Some("local") => {
                let l = worktree::local(&dir)?;
                println!("ref         local {} from {}", l.content, dir.display());
                println!("            {}", if l.dirty {
                    "uncommitted changes are included and are in that hash"
                } else { "clean, so this is the commit as it stands" });
            }
            r => println!("ref         {}", r.unwrap_or("HEAD")),
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
        for (i, s) in rec.steps.iter().enumerate() {
            println!("step {}      [{:?}] {}", i + 1, s.lock, s.run);
            for (k, v) in &s.env {
                println!("            env {k}={v}");
            }
            println!("            label {}", step_labels[i]);
        }
        return Ok(ExitCode::SUCCESS);
    }

    // A recipe with any exclusive step is a measurement, and a measurement goes where it is
    // told: its history keys on the machine, and bindings that would make moving one safe do
    // not exist yet. So only a wholly shared recipe, which is every build and test, is ranked.
    //
    // Both paths then claim the repo's build cache for the machine they chose, and a
    // measurement's claim is the one that sticks because it is the one that could not move.
    // Without that, a build ranked onto one machine leaves the benchmark on another to compile
    // inside its own exclusive lock, which is what splitting build from measure prevents.
    let mut backend = Dibs::default();
    backend.machine = if std::env::var("DIBS_ROUTE").as_deref() == Ok("1")
        && rec.steps.iter().all(|s| s.lock == Lock::Shared)
    {
        Dibs::routed(&backend.program, affinity_get(&repo_name).as_deref(), Some(&repo_name))
    } else {
        Dibs::which(&backend.program)
    };
    if let Some(m) = &backend.machine {
        affinity_set(&repo_name, m);
    }

    // The worktree comes first and takes the shared lock, because a fetch and a checkout are
    // work that tolerates neighbours. Doing it inside a measured step would put a git fetch
    // inside the exclusive hold.
    let reference = args.reference.as_deref().unwrap_or("HEAD");
    // A branch that was never pushed has no fetchable ref, and refusing to push a perf branch
    // just to measure it is not misuse. Without this the answer was to hand-roll sync and
    // build, which loses the cache isolation, the recorded revision and the lock split all at
    // once, and the two wrong numbers that produced were both in the part that got rewritten.
    let local = if reference == "local" { Some(worktree::local(&dir)?) } else { None };
    match &local {
        Some(l) => eprintln!(
            "dibs: preparing {repo_name} from {} ({})",
            dir.display(),
            if l.dirty { "uncommitted changes included" } else { "clean" }
        ),
        None => eprintln!("dibs: preparing {repo_name}@{reference}"),
    }
    let signature = rec.steps.iter().find_map(|st| worktree::build_signature(&st.run)).unwrap_or_default();
    let token = new_token();
    let (script, gitdbs) = tree_script(&dir, &repo_name, reference, local.as_ref(), &signature, &token);

    let fold = local.is_none() && rec.steps[0].lock == Lock::Shared;
    let first_step_call = calls.len() - rec.steps.len();
    let own_batch = batch::batch_id();
    let env_of = |k: usize| batch::recipe_env(&own_batch, &calls, k);
    let setup_env = env_of(0);
    let setup = Request {
        label: &calls[0].label,
        lock: Lock::Shared,
        isolation: rec.isolation,
        needs: None,
        // Preparing a worktree touches no GPU, so pinning it would only make the setup fail
        // on a machine whose card has been pulled.
        device: None,
        env: &setup_env,
        max: None,
    };
    // One cache per repo, exported rather than left to each recipe to remember. The output
    // needs no file of its own: dibs keeps every job's log under its job id, and a path named
    // after the label would be shared by two runs of one recipe.
    let run_of = |i: usize| step_command(rec, i, &token, args.anyway);
    let mut announce = |text: &str| announce_prepared(text);

    let mut steps = Vec::new();
    let mut failed = None;
    let text = match &local {
        Some(l) => {
            let (out, text) = sync_prepared(&backend, &dir, &script, &l.key, &setup, &mut announce)?;
            if !text.contains("DIBS-READY") {
                return Err(format!("could not prepare {repo_name} from {} (exit {})", dir.display(), out.status));
            }
            if out.status != 0 {
                return Err(format!("sending {} failed (exit {})", dir.display(), out.status));
            }
            text
        }
        None if fold => {
            eprintln!("dibs: step 1/{} [{:?}], with the setup ahead of it", rec.steps.len(), rec.steps[0].lock);
            let env = env_of(first_step_call);
            let command = worktree::ahead(&script, worktree::Then::Step, &rec.steps[0].run) + &format!("{{ {}; }}", run_of(0));
            let req = Request {
                label: &step_labels[0],
                lock: rec.steps[0].lock,
                isolation: rec.isolation,
                needs: rec.needs.as_deref(),
                device: args.device.as_deref(),
                env: &env,
                max: args.max,
            };
            let (out, text) = backend.run_reporting(&req, &command, &mut announce)?;
            if !text.contains("DIBS-READY") && !text.contains("DIBS-HELD") {
                return Err(format!("could not prepare {repo_name}@{reference} (exit {})", out.status));
            }
            if text.contains("DIBS-READY") {
                steps.push(provenance::StepRecord { lock: step_lock(rec.steps[0].lock), status: out.status, seconds: out.seconds });
                if out.status != 0 {
                    failed = Some(out.status);
                }
            }
            text
        }
        None => {
            let (out, text) = backend.run_capture(&setup, &script)?;
            if out.status != 0 {
                return Err(format!("could not prepare {repo_name}@{reference} (exit {})", out.status));
            }
            announce(&text);
            text
        }
    };
    let prepared = worktree::parse(&text)?;
    send_missing_gitdbs(&backend, &text, &gitdbs);

    for (i, step) in rec.steps.iter().enumerate().skip(steps.len()) {
        if failed.is_some() {
            break;
        }
        let cd = format!(
            "cd {} && export CARGO_TARGET_DIR={} && {{ {}; }}",
            sh(&prepared.worktree),
            sh(&prepared.target),
            run_of(i)
        );
        eprintln!("dibs: step {}/{} [{:?}]", i + 1, rec.steps.len(), step.lock);
        // The step says which lock it wants, where it can be reviewed, instead of a compile
        // being invisible inside a script that holds the machine exclusively.
        let env = env_of(first_step_call + i);
        let req = Request {
            label: &step_labels[i],
            lock: step.lock,
            isolation: rec.isolation,
            needs: rec.needs.as_deref(),
            device: args.device.as_deref(),
            env: &env,
            max: args.max,
        };
        let (out, report) = backend.run_reporting(&req, &cd, &mut |_| {})?;
        // The machine has said why; a refusal is not a run, so it leaves no record.
        if report.lines().any(|l| l == "DIBS-REFUSED") {
            return Ok(ExitCode::from(78));
        }
        steps.push(provenance::StepRecord { lock: step_lock(step.lock), status: out.status, seconds: out.seconds });
        if out.status != 0 {
            failed = Some(out.status);
        }
    }

    let record = provenance::Run {
        label,
        // shell borrows Build's machinery but is not a build, and a record that says
        // otherwise is a record that misleads whoever reads it later.
        verb: if shell_reason.is_some() { "shell" } else { verb.as_str() },
        recipe: name.to_string(),
        fingerprint,
        isolation: format!("{:?}", rec.isolation).to_lowercase(),
        needs: rec.needs.clone(),
        reason: shell_reason.clone(),
        procedure: rec
            .steps
            .iter()
            // With what it exported, since a recipe in local config cannot be recovered by
            // checking out a ref and an environment variable changes what was measured.
            .map(|st| {
                let exports: String =
                    st.env.iter().map(|(k, v)| format!("export {k}={}; ", sh(v))).collect();
                (format!("{:?}", st.lock).to_lowercase(), format!("{exports}{}", st.run))
            })
            .collect(),
        params,
        backend: backend.name(),
        device: args.device.clone(),
        machine: backend.machine.clone(),
        // Read on the machine, from the tree that was actually built, rather than from a
        // checkout here that may be at a different commit entirely.
        revisions: prepared.revisions.clone(),
        seeded: prepared.seeded.clone(),
        steps,
    };
    write_record(&record)?;

    Ok(match failed {
        Some(c) => ExitCode::from(c.clamp(1, 255) as u8),
        None => ExitCode::SUCCESS,
    })
}

/// Every combination `--sweep` asks for, each a complete set of values for one run. Without a
/// sweep this is the one point the call already described.
fn sweep_points(args: &Args) -> Vec<BTreeMap<String, String>> {
    let mut points = vec![args.params.clone()];
    for (name, values) in &args.sweep {
        points = points
            .iter()
            .flat_map(|p| {
                values.iter().map(|v| {
                    let mut q = p.clone();
                    q.insert(name.clone(), v.clone());
                    q
                })
            })
            .collect();
    }
    points
}

/// A sweep is a batch of ordinary calls, which is what makes it one wake and one summary rather
/// than one per point. They run in sequence because they share a worktree and its build cache.
fn sweep_run(args: &Args, points: &[BTreeMap<String, String>]) -> Result<ExitCode, String> {
    // Every point is checked before any of them is queued: a value the recipe refuses should be
    // found now, not two measurements into a sweep that is already holding the machine.
    for p in points {
        let probe = Args { params: p.clone(), sweep: Vec::new(), reps: 1, ..args.clone() };
        resolve(&probe)?;
    }
    let text = sweep_text(args, points);
    let code = batch::run(&text, &batch::Options { dry_run: args.dry_run, verbose: args.verbose })?;
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// The batch a sweep becomes: one ordinary dibs call per point, named by what makes it that
/// point, in the order the sweep was written.
fn sweep_text(args: &Args, points: &[BTreeMap<String, String>]) -> String {
    let target = match &args.reference {
        Some(r) => format!("{}@{r}", args.repo),
        None => args.repo.clone(),
    };
    let mut text = String::new();
    for p in points {
        for rep in 1..=args.reps {
            let mut line = format!("dibs {} {}", args.verb, sh(&target));
            if let Some(r) = &args.recipe {
                line += &format!(" {r}");
            }
            if args.bench {
                line += " --bench";
            }
            if let Some(m) = args.max {
                line += &format!(" --max {m}");
            }
            if args.anyway {
                line += " --anyway";
            }
            if let Some(d) = &args.device {
                line += &format!(" --device {d}");
            }
            if let Some(r) = &args.reason {
                line += &format!(" --reason {}", sh(r));
            }
            for (k, v) in p {
                line += &format!(" --{k} {}", sh(v));
            }
            if let Some(c) = &args.command {
                line += &format!(" -- {}", sh(c));
            }
            text += &format!("[{}] {line}\n", point_name(args, p, rep));
        }
    }
    text
}

/// What the summary calls one point: the values that make it that point, and the repetition
/// where there is more than one.
fn point_name(args: &Args, p: &BTreeMap<String, String>, rep: u32) -> String {
    let slug = |v: &str| {
        v.chars().map(|c| if c.is_ascii_alphanumeric() || "_.-".contains(c) { c } else { '_' }).collect::<String>()
    };
    let mut parts: Vec<String> = args
        .sweep
        .iter()
        .map(|(k, _)| format!("{k}-{}", slug(p.get(k).map(String::as_str).unwrap_or(""))))
        .collect();
    if args.reps > 1 {
        parts.push(format!("r{rep}"));
    }
    parts.join(".")
}

/// A repo's servers, running on the machine under one lock while the command runs here: a
/// dashboard, a client, a test suite driving them over the network. It ends by becoming that
/// dibs call rather than waiting on one, so the command keeps this terminal.
fn with_service(args: &Args) -> Result<ExitCode, String> {
    let dir = resolve_repo(&args.repo, &args.root)?;
    let repo_name = worktree::identity(&dir);
    let manifest = Manifest::load(&dir, &repo_name)?;
    let name = args
        .recipe
        .as_deref()
        .ok_or_else(|| format!("with needs a service: dibs with {} <service> -- <command>", args.repo))?;
    let svc = manifest.service(name).ok_or_else(|| {
        let have: Vec<&str> = manifest.service_listing().iter().map(|(n, _)| *n).collect();
        match have.is_empty() {
            true => format!("{repo_name} defines no services, so there is nothing to run against"),
            false => format!("no service '{name}' for {repo_name}. It has: {}", have.join(", ")),
        }
    })?;
    if svc.serves.is_empty() {
        return Err(format!("service '{name}' starts nothing: it needs a [[service.{name}.serve]] with a run"));
    }
    let command = args.command.as_deref().ok_or("with needs a command after --")?;

    let mut backend = Dibs::default();
    backend.machine = Dibs::which(&backend.program);
    if let Some(m) = &backend.machine {
        affinity_set(&repo_name, m);
    }
    let label = run_label(&repo_name, "with", Some(name), args.device.as_deref());

    let reference = args.reference.as_deref().unwrap_or("HEAD");
    let local = if reference == "local" { Some(worktree::local(&dir)?) } else { None };
    match &local {
        Some(l) => eprintln!(
            "dibs: preparing {repo_name} from {} ({})",
            dir.display(),
            if l.dirty { "uncommitted changes included" } else { "clean" }
        ),
        None => eprintln!("dibs: preparing {repo_name}@{reference}"),
    }
    let signature = svc.build.as_deref().and_then(worktree::build_signature).unwrap_or_default();
    let (script, gitdbs) = tree_script(&dir, &repo_name, reference, local.as_ref(), &signature, &new_token());
    let setup_label = format!("{label}:{}", if local.is_some() { "send" } else { "setup" });
    let setup = Request {
        label: &setup_label,
        lock: Lock::Shared,
        isolation: recipe::Isolation::Machine,
        needs: None,
        device: None,
        env: &[],
        max: None,
    };
    let mut announce = |text: &str| announce_prepared(text);
    let text = match &local {
        Some(l) => {
            let (out, text) = sync_prepared(&backend, &dir, &script, &l.key, &setup, &mut announce)?;
            if !text.contains("DIBS-READY") || out.status != 0 {
                return Err(format!("could not prepare {repo_name} from {} (exit {})", dir.display(), out.status));
            }
            text
        }
        None => {
            let (out, text) = backend.run_capture(&setup, &script)?;
            if out.status != 0 {
                return Err(format!("could not prepare {repo_name}@{reference} (exit {})", out.status));
            }
            announce(&text);
            text
        }
    };
    let prepared = worktree::parse(&text)?;
    send_missing_gitdbs(&backend, &text, &gitdbs);

    // In the tree, with the repo's build cache, exactly as a recipe step runs.
    let in_tree = |run: &str| {
        format!("cd {} && export CARGO_TARGET_DIR={} && {{ {run}; }}", sh(&prepared.worktree), sh(&prepared.target))
    };
    if let Some(build) = &svc.build {
        eprintln!("dibs: building {name}");
        let build_label = format!("{label}:build");
        let req = Request {
            label: &build_label,
            lock: Lock::Shared,
            isolation: recipe::Isolation::Machine,
            needs: None,
            device: args.device.as_deref(),
            env: &[],
            max: None,
        };
        let build = match worktree::build_signature(build) {
            Some(_) => worktree::claiming(build),
            None => build.clone(),
        };
        let out = backend.run(&req, &in_tree(&build))?;
        if out.status != 0 {
            return Ok(ExitCode::from(out.status.clamp(1, 255) as u8));
        }
    }

    let mut cmd = std::process::Command::new(&backend.program);
    if let Some(m) = &backend.machine {
        cmd.arg("--on").arg(m);
    }
    cmd.arg("--hold").arg("--label").arg(&label);
    if let Some(d) = &args.device {
        cmd.arg("--device").arg(d);
    }
    for p in &svc.ports {
        cmd.arg("--port").arg(p);
    }
    for serve in &svc.serves {
        cmd.arg("--with").arg(format!("{}={}", serve.name, in_tree(&serve.run)));
        if let Some(ready) = &serve.ready {
            cmd.arg("--ready").arg(ready);
        }
    }
    cmd.arg("--").arg(command);
    Err(format!("could not run {}: {}", backend.program, exec(cmd)))
}

/// What step `i` runs in its tree. A build claims the target for this tree, and a measurement
/// after one refuses a target some other tree has built into since, unless told `anyway`.
fn step_command(rec: &recipe::Recipe, i: usize, token: &str, anyway: bool) -> String {
    let step = &rec.steps[i];
    let run = match (worktree::build_signature(&step.run), step.lock) {
        (Some(_), Lock::Shared) => worktree::claiming(&worktree::recording(&step.run, token)),
        (Some(_), Lock::Exclusive) => worktree::recording(&step.run, token),
        (None, _) => step.run.clone(),
    };
    let built = rec.steps[..i].iter().any(|s| s.lock == Lock::Shared && worktree::build_signature(&s.run).is_some());
    let run = match step.lock == Lock::Exclusive && built && !anyway {
        true => worktree::checked(&run),
        false => run,
    };
    // Exported rather than prefixed onto the command, so it reaches a pipeline or a loop in the
    // step as well as the first word of it.
    let exports: String = step.env.iter().map(|(k, v)| format!("export {k}={}; ", sh(v))).collect();
    format!("{exports}{run}")
}

/// The script that prepares the tree on the machine, and the git databases it may need sent.
fn tree_script(
    dir: &Path,
    repo_name: &str,
    reference: &str,
    local: Option<&worktree::Local>,
    signature: &str,
    token: &str,
) -> (String, Vec<gitdeps::Db>) {
    let lock = match local {
        Some(_) => std::fs::read_to_string(dir.join("Cargo.lock")).ok(),
        None => std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["show", &format!("{reference}:Cargo.lock")])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()),
    };
    let gitdbs = gitdeps::local(&gitdeps::cargo_home(), &gitdeps::pinned(lock.as_deref().unwrap_or("")));
    let script = worktree::packages_script(lock.as_deref().unwrap_or(""), signature, token)
        + &match local {
            Some(l) => worktree::setup_local_script(repo_name, &l.key, &l.content),
            None => worktree::setup_script(repo_name, reference),
        }
        + &gitdeps::check_script(&gitdbs);
    (script, gitdbs)
}

/// Unique per invocation, and what a build's package list is staged under until it succeeds.
fn new_token() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    )
}

fn send_missing_gitdbs(backend: &Dibs, text: &str, gitdbs: &[gitdeps::Db]) {
    let (Some(remote), gone) = gitdeps::missing(text, gitdbs) else { return };
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

/// Becomes the command, so it keeps this terminal: prompts, Ctrl-C and the exit status are the
/// command's own rather than something relayed.
fn exec(mut cmd: std::process::Command) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    cmd.exec()
}

/// A recipe invocation resolved as far as it can be without a machine: which recipe, and the
/// labels its jobs are filed under.
struct Resolved {
    dir: PathBuf,
    repo_name: String,
    verb: Verb,
    name: String,
    rec: recipe::Recipe,
    label: String,
    step_labels: Vec<String>,
    shell_reason: Option<String>,
    params: BTreeMap<String, String>,
}

fn resolve(args: &Args) -> Result<Resolved, String> {
    let dir = resolve_repo(&args.repo, &args.root)?;
    let repo_name = worktree::identity(&dir);
    let manifest = if args.verb == "shell" {
        Manifest::default()
    } else {
        Manifest::load(&dir, &repo_name)?
    };


    let shell_reason = if args.verb == "shell" {
        Some(args.reason.clone().ok_or(
            "shell needs --reason. Most of what gets run is neither a build nor a benchmark,\n             and knowing what those were is how the next recipe gets written.",
        )?)
    } else {
        None
    };
    let shell_recipe = shell_reason.as_ref().map(|_| recipe::Recipe {
        source: recipe::Source::Local,
        needs: None,
        isolation: recipe::Isolation::Machine,
        params: BTreeMap::new(),
        steps: vec![recipe::Step {
            lock: if args.bench { Lock::Exclusive } else { Lock::Shared },
            run: args.command.clone().unwrap_or_default(),
            env: BTreeMap::new(),
        }],
    });
    if shell_recipe.is_some() && args.command.is_none() {
        return Err("shell needs -- <command>".into());
    }

    let verb = Verb::parse(&args.verb).or(if args.verb == "shell" {
        Some(Verb::Build)
    } else {
        None
    })
    .ok_or_else(|| {
        format!("not a verb: {} (build, test, bench, shell, raw, list, runs or gaps)", args.verb)
    })?;
    let name = if shell_recipe.is_some() { Some("shell") } else { args.recipe.as_deref() }
        .ok_or_else(|| {
        let have = manifest.names(verb);
        if have.is_empty() {
            format!("{} defines no {} recipes", dir.display(), verb.as_str())
        } else {
            format!("needs a recipe name; {} has: {}", dir.display(), have.join(", "))
        }
        })?;
    let mut rec = shell_recipe.clone().map(Ok).unwrap_or_else(|| manifest.recipe(verb, name).cloned().ok_or_else(|| {
        let have = manifest.names(verb);
        format!(
            "no {} recipe called '{name}'; {} has: {}",
            verb.as_str(),
            dir.display(),
            if have.is_empty() { "none".into() } else { have.join(", ") }
        )
    }))?;
    if rec.steps.is_empty() {
        return Err(format!("recipe '{name}' declares no steps"));
    }
    let params = rec.values(&args.params).map_err(|e| format!("{name}: {e}"))?;
    rec = rec.bound(&params);
    rec.check(name)?;

    // Derived, never supplied. A label an agent writes by hand names the run rather than the
    // kind of work, which is why 51 of 80 labels in the old history appeared exactly once and
    // filed their duration where nothing would look it up again.
    //
    // The verb is in it because a recipe name is only unique within a verb: `build cubek cuda`
    // and `test cubek cuda` are different work, and one history for both predicts each from
    // the other. Shell has no recipe name to carry.
    let label = match &shell_recipe {
        Some(_) => run_label(&repo_name, "shell", None, args.device.as_deref()),
        None => run_label(&repo_name, verb.as_str(), Some(name), args.device.as_deref()),
    };
    // The duration history keys on lock and label together, so a recipe's build and its
    // measurement stay apart on their own. Two steps taking the *same* lock would not, and
    // their durations would average into one meaningless number: the bimodal history that
    // made estimates useless in the first place, rebuilt deliberately.
    let step_labels = label_steps(&label, &rec.steps);
    Ok(Resolved {
        dir,
        repo_name,
        verb,
        name: name.to_string(),
        rec,
        label,
        step_labels,
        shell_reason,
        params,
    })
}

/// The jobs a recipe makes, in order, under the labels their durations are filed by. The setup
/// rides at the head of the first job that needs the tree: the transfer for a local tree, or the
/// first step when it is shared. Its own job would be a second round trip and a second place in
/// the queue. An exclusive first step keeps its setup apart, or a fetch would run inside the hold.
fn jobs_of(r: &Resolved, local: bool) -> Vec<batch::Pending> {
    let job = |suffix: &str, mode: &'static str| {
        let label = format!("{}{suffix}", r.label);
        batch::Pending { name: label.clone(), mode, label, here: true }
    };
    let mut jobs = Vec::new();
    if local {
        jobs.push(job(":send", "rsh"));
    } else if r.rec.steps[0].lock != Lock::Shared {
        jobs.push(job(":setup", "shared"));
    }
    for (i, st) in r.rec.steps.iter().enumerate() {
        let mode = match st.lock {
            Lock::Shared => "shared",
            Lock::Exclusive => "bench",
        };
        jobs.push(batch::Pending { name: r.step_labels[i].clone(), mode, label: r.step_labels[i].clone(), here: true });
    }
    jobs
}

/// The jobs a recipe line in a batch will make, so the batch's plan can estimate them. None
/// when the line does not resolve here, which leaves that step without an estimate.
fn recipe_jobs(words: &[String]) -> Option<Vec<batch::Pending>> {
    if words.iter().any(|w| matches!(w.as_str(), "-h" | "--help" | "--version")) {
        return None;
    }
    let mut i = 1;
    while i < words.len() && words[i].starts_with('-') {
        i += if words[i] == "--on" { 2 } else { 1 };
    }
    let args = parse_words(words.get(i..)?.iter().cloned()).ok()?;
    let r = resolve(&args).ok()?;
    Some(jobs_of(&r, args.reference.as_deref() == Some("local")))
}

fn run_label(repo: &str, verb: &str, name: Option<&str>, device: Option<&str>) -> String {
    let base = match name {
        Some(n) => format!("{repo}/{verb}/{n}"),
        None => format!("{repo}/{verb}"),
    };
    // On a machine with one card the device adds nothing, and on a machine with four it is
    // the difference between four histories and one. Without it a recipe named one series
    // per card, so the second card was refused and --new-series answered by discarding the
    // first: two cards could be measured, never both kept.
    match device {
        Some(d) => format!("{base}@{d}"),
        None => base,
    }
}

/// One label per step, suffixed only where it has to be. A recipe with a build and a
/// measurement needs no suffix, because the lock already separates them.
fn label_steps(base: &str, steps: &[recipe::Step]) -> Vec<String> {
    let mut out = Vec::with_capacity(steps.len());
    for (i, s) in steps.iter().enumerate() {
        let same = steps.iter().filter(|o| o.lock == s.lock).count();
        if same > 1 {
            out.push(format!("{base}.{}", i + 1));
        } else {
            out.push(base.to_string());
        }
    }
    out
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
fn sync_prepared(
    backend: &Dibs,
    from: &Path,
    setup: &str,
    key: &str,
    req: &Request,
    on_report: &mut dyn FnMut(&str),
) -> Result<(resource::Outcome, String), String> {
    let before = std::env::temp_dir().join(format!("dibs-before.{}.{key}", std::process::id()));
    std::fs::write(&before, worktree::ahead(setup, worktree::Then::Transfer, &format!("prepare, then receive {}", from.display())))
        .map_err(|e| format!("{}: {e}", before.display()))?;
    let mut cmd = std::process::Command::new(&backend.program);
    if let Some(m) = &backend.machine {
        cmd.arg("--on").arg(m);
    }
    cmd.arg("--label")
        .arg(req.label)
        .arg("--sync")
        .args(worktree::SYNC_ARGS)
        .arg(format!("{}/", from.display()))
        .arg(format!(":local-{key}/"))
        .env("DIBS_FROM_RUN", "1")
        .env("DIBS_SYNC_BEFORE", &before)
        .envs(req.env.iter().map(|(k, v)| (*k, v)))
        .stdin(std::process::Stdio::null());
    let result = resource::reporting(cmd, on_report);
    let _ = std::fs::remove_file(&before);
    result
}

fn announce_prepared(text: &str) {
    let Ok(prepared) = worktree::parse(text) else { return };
    eprintln!("dibs: {}", prepared.worktree);
    if let Some(from) = &prepared.seeded {
        match prepared.seed_shared {
            Some((have, of)) => eprintln!(
                "dibs: target directory copied from {from}, whose builds match {have} of the {of} groups in this tree's lockfile{}",
                if prepared.seeded_sources { ", with its sources so unchanged crates stay built" } else { "" }
            ),
            None => eprintln!("dibs: target directory copied from {from}, so only what differs rebuilds"),
        }
    }
}

fn step_lock(lock: Lock) -> &'static str {
    match lock {
        Lock::Shared => "shared",
        Lock::Exclusive => "exclusive",
    }
}

/// Adds files and never replaces one: git names objects by their content, so what is already
/// there is already right, and a cargo on the machine may be reading it.
fn sync_gitdb(backend: &Dibs, from: &Path, to: &str) -> Result<(), String> {
    let mut cmd = std::process::Command::new(&backend.program);
    if let Some(m) = &backend.machine {
        cmd.arg("--on").arg(m);
    }
    cmd.arg("--sync")
        .arg("-a")
        .arg("--ignore-existing")
        .arg(format!("{}/", from.display()))
        .arg(format!(":{to}/"))
        .env("DIBS_FROM_RUN", "1")
        .stdin(std::process::Stdio::null());
    let st = cmd.status().map_err(|e| format!("dibs --sync: {e}"))?;
    if !st.success() {
        return Err(format!("sending {} failed", from.display()));
    }
    Ok(())
}

fn resolve_repo(repo: &str, root: &Path) -> Result<PathBuf, String> {
    let direct = PathBuf::from(repo);
    if direct.join(".dibs.toml").exists() || direct.join(".git").exists() {
        return canon(direct);
    }
    let under = root.join(repo);
    if under.exists() {
        return canon(under);
    }
    Err(format!(
        "no repo at '{repo}' and none at {}/{repo}.\n  A bare name is looked up under DIBS_ROOT, then --root, then the `root` key of\n  ~/.config/dibs/machines.toml, then the current directory. Give a path, or set one of those.",
        root.display()
    ))
}

fn canon(p: PathBuf) -> Result<PathBuf, String> {
    p.canonicalize().map_err(|e| format!("{}: {e}", p.display()))
}

/// Which machine holds a repo's build cache. Kept beside the run record, on this side, since
/// it describes the pool rather than any one machine in it.
fn affinity_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/dibs/affinity"))
}

fn affinity_get(repo: &str) -> Option<String> {
    let text = std::fs::read_to_string(affinity_path()?).ok()?;
    text.lines()
        .filter_map(|l| l.split_once('\t'))
        .find(|(r, _)| *r == repo)
        .map(|(_, m)| m.trim().to_string())
}

fn affinity_set(repo: &str, machine: &str) {
    let Some(p) = affinity_path() else { return };
    let mut kept: Vec<String> = std::fs::read_to_string(&p)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.split_once('\t').map(|(r, _)| r != repo).unwrap_or(false))
        .map(str::to_string)
        .collect();
    kept.push(format!("{repo}\t{machine}"));
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kept.join("\n") + "\n");
}

fn runs_path() -> Result<PathBuf, String> {
    match std::env::var_os("DIBS_RUNS") {
        Some(p) => Ok(PathBuf::from(p)),
        None => {
            let home = std::env::var_os("HOME").ok_or("no HOME, and nowhere to record runs")?;
            Ok(PathBuf::from(home).join(".local/state/dibs/runs.jsonl"))
        }
    }
}

fn write_record(run: &provenance::Run) -> Result<(), String> {
    let when = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = runs_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{}", run.to_json(when)).map_err(|e| format!("{}: {e}", path.display()))
}

fn sh(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || "/._-@".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recipe::{Isolation, Recipe, Step};

    fn step(lock: Lock, run: &str) -> Step {
        Step { lock, run: run.into(), env: BTreeMap::new() }
    }

    fn swept(words: &[&str]) -> Args {
        parse_words(words.iter().map(|w| w.to_string())).unwrap()
    }

    #[test]
    fn a_sweep_is_every_combination_over_the_values_already_given() {
        let args = swept(&[
            "bench", "app@local", "r", "--backend", "cuda", "--sweep", "size=64,128", "--sweep",
            "layout=rc,cr",
        ]);
        let points = sweep_points(&args);
        let shape: Vec<String> =
            points.iter().map(|p| format!("{} {} {}", p["backend"], p["size"], p["layout"])).collect();
        assert_eq!(shape, ["cuda 64 rc", "cuda 64 cr", "cuda 128 rc", "cuda 128 cr"]);
    }

    #[test]
    fn a_value_with_a_comma_in_it_is_one_value() {
        let args = swept(&["bench", "app@local", "r", "--problems", "topk1,topk2", "--sweep", "samples=10,30"]);
        let points = sweep_points(&args);
        assert_eq!(points.len(), 2, "only the sweep multiplies the runs");
        assert_eq!(points[0]["problems"], "topk1,topk2");
    }

    #[test]
    fn a_swept_run_is_a_batch_of_ordinary_calls() {
        let args = swept(&[
            "bench", "app@local", "r", "--device", "gpu0", "--sweep", "samples=10,30", "--reps", "2", "--anyway",
        ]);
        let text = sweep_text(&args, &sweep_points(&args));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "two values, twice each");
        assert_eq!(lines[0], "[samples-10.r1] dibs bench app@local r --anyway --device gpu0 --samples 10");
        assert_eq!(lines[3], "[samples-30.r2] dibs bench app@local r --anyway --device gpu0 --samples 30");
    }

    #[test]
    fn a_repeated_shell_carries_its_reason_its_lock_and_its_command_quoted() {
        let args = swept(&[
            "shell", "app@local", "--reason", "why not", "--bench", "--max", "60", "--reps", "2",
            "--", "echo a; echo b",
        ]);
        let text = sweep_text(&args, &sweep_points(&args));
        assert_eq!(
            text.lines().next().unwrap(),
            "[r1] dibs shell app@local --bench --max 60 --reason 'why not' -- 'echo a; echo b'",
            "a batch line is one dibs call, so anything the shell would read has to be quoted"
        );
    }

    fn resolved(steps: Vec<Step>) -> Resolved {
        let rec = Recipe { source: recipe::Source::Local, needs: None, isolation: Isolation::Machine, params: BTreeMap::new(), steps };
        let step_labels = label_steps("app/bench/r", &rec.steps);
        Resolved {
            dir: PathBuf::from("."),
            repo_name: "app".into(),
            verb: Verb::Bench,
            name: "r".into(),
            rec,
            label: "app/bench/r".into(),
            step_labels,
            shell_reason: None,
            params: BTreeMap::new(),
        }
    }

    #[test]
    fn a_build_claims_its_target_and_the_measurement_after_it_checks_the_claim() {
        let rec = resolved(vec![step(Lock::Shared, "cargo build --release"), step(Lock::Exclusive, "cargo bench")]).rec;
        assert_eq!(step_command(&rec, 0, "t", false), worktree::claiming(&worktree::recording("cargo build --release", "t")));
        assert_eq!(step_command(&rec, 1, "t", false), worktree::checked(&worktree::recording("cargo bench", "t")));
        assert_eq!(step_command(&rec, 1, "t", true), worktree::recording("cargo bench", "t"));
    }

    // Nothing was built into the target, so whatever claimed it last says nothing about this run.
    #[test]
    fn a_measurement_after_no_build_is_not_checked() {
        let rec = resolved(vec![step(Lock::Shared, "make data"), step(Lock::Exclusive, "./bench.sh")]).rec;
        assert_eq!(step_command(&rec, 1, "t", false), "./bench.sh");
    }

    #[test]
    fn a_recipe_s_jobs_are_what_it_will_send_and_run() {
        let names = |jobs: Vec<batch::Pending>| jobs.into_iter().map(|j| format!("{} {}", j.mode, j.label)).collect::<Vec<_>>();
        let build_then_bench = resolved(vec![step(Lock::Shared, "cargo build"), step(Lock::Exclusive, "cargo bench")]);
        assert_eq!(names(jobs_of(&build_then_bench, true)), ["rsh app/bench/r:send", "shared app/bench/r", "bench app/bench/r"]);
        assert_eq!(names(jobs_of(&build_then_bench, false)), ["shared app/bench/r", "bench app/bench/r"], "the setup rides with the build");
        let bench_only = resolved(vec![step(Lock::Exclusive, "cargo bench")]);
        assert_eq!(names(jobs_of(&bench_only, false)), ["shared app/bench/r:setup", "bench app/bench/r"], "never inside the hold");
    }

    #[test]
    fn a_build_and_a_measurement_need_no_suffix() {
        let steps = vec![step(Lock::Shared, "build"), step(Lock::Exclusive, "bench")];
        assert_eq!(label_steps("r/x", &steps), vec!["r/x", "r/x"]);
    }

    #[test]
    fn a_recipe_name_is_only_unique_within_its_verb() {
        assert_ne!(
            run_label("cubek", "build", Some("cuda"), None),
            run_label("cubek", "test", Some("cuda"), None)
        );
    }

    #[test]
    fn two_steps_taking_the_same_lock_must_not_share_a_label() {
        let steps = vec![
            step(Lock::Shared, "one"),
            step(Lock::Shared, "two"),
            step(Lock::Exclusive, "measure"),
        ];
        assert_eq!(label_steps("r/x", &steps), vec!["r/x.1", "r/x.2", "r/x"]);
    }

    #[test]
    fn the_fingerprint_follows_the_procedure_and_nothing_else() {
        let a = Recipe {
            source: recipe::Source::Repo,
            needs: None,
            isolation: Isolation::Machine,
            params: BTreeMap::new(),
            steps: vec![step(Lock::Shared, "cargo build")],
        };
        let same = Recipe {
            source: recipe::Source::Repo,
            needs: None,
            isolation: Isolation::Machine,
            params: BTreeMap::new(),
            steps: vec![step(Lock::Shared, "cargo build")],
        };
        let changed_command = Recipe {
            source: recipe::Source::Repo,
            needs: None,
            isolation: Isolation::Machine,
            params: BTreeMap::new(),
            steps: vec![step(Lock::Shared, "cargo build --release")],
        };
        let changed_lock = Recipe {
            source: recipe::Source::Repo,
            needs: None,
            isolation: Isolation::Machine,
            params: BTreeMap::new(),
            steps: vec![step(Lock::Exclusive, "cargo build")],
        };
        assert_eq!(a.fingerprint(), same.fingerprint());
        assert_ne!(a.fingerprint(), changed_command.fingerprint());
        // A step moved from the shared lock to the exclusive one is a different procedure
        // even though it runs the same command, and comparing across it would be wrong.
        assert_ne!(a.fingerprint(), changed_lock.fingerprint());
    }

    #[test]
    fn isolation_defaults_to_the_whole_machine() {
        let r: Recipe = toml::from_str("[[step]]\nlock = \"shared\"\nrun = \"x\"").unwrap();
        assert_eq!(r.isolation, Isolation::Machine);
    }

    // It was set on every run and serialized by nothing, so every record said the machine and
    // none said the card. The compiler called the field dead and was right.
    #[test]
    fn the_card_a_run_used_reaches_the_record() {
        let run = provenance::Run {
            label: "cubecl/bench/throughput-all@gpu:rtx2060".into(),
            verb: "bench",
            recipe: "throughput-all".into(),
            fingerprint: "abc".into(),
            isolation: "machine".into(),
            needs: None,
            reason: None,
            procedure: vec![],
            params: BTreeMap::new(),
            backend: "dibs",
            device: Some("gpu:rtx2060".into()),
            machine: Some("multigpu".into()),
            revisions: vec![],
            seeded: None,
            steps: vec![],
        };
        let v: serde_json::Value = serde_json::from_str(&run.to_json(1)).expect("valid json");
        assert_eq!(v["device"], "gpu:rtx2060");
        assert_eq!(v["machine"], "multigpu");
    }

    #[test]
    fn a_card_gets_its_own_label_and_an_unpinned_run_is_left_alone() {
        let pinned = run_label("cubecl", "bench", Some("throughput-all"), Some("gpu:a"));
        let other = run_label("cubecl", "bench", Some("throughput-all"), Some("gpu:b"));
        assert_ne!(pinned, other);
        assert_eq!(
            run_label("cubecl", "bench", Some("throughput-all"), None),
            "cubecl/bench/throughput-all"
        );
    }

    #[test]
    fn a_label_with_a_quote_in_it_cannot_break_the_record() {
        let run = provenance::Run {
            label: "r/\"x\\y\nz".into(),
            verb: "bench",
            recipe: "x".into(),
            fingerprint: "abc".into(),
            isolation: "machine".into(),
            needs: None,
            reason: None,
            procedure: vec![],
            params: BTreeMap::new(),
            backend: "dibs",
            device: None,
            machine: None,
            revisions: vec![],
            seeded: None,
            steps: vec![],
        };
        let line = run.to_json(1);
        assert!(!line.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(v["label"], "r/\"x\\y\nz");
    }

    #[test]
    fn a_run_record_is_one_line_of_valid_json() {
        let run = provenance::Run {
            label: "r/x".into(),
            verb: "bench",
            recipe: "x".into(),
            fingerprint: "abc".into(),
            isolation: "machine".into(),
            needs: Some("gpu, num_tensor_cores >= 1".into()),
            reason: None,
            procedure: vec![("shared".into(), "cargo build".into())],
            params: BTreeMap::new(),
            backend: "dibs",
            device: None,
            machine: None,
            revisions: vec![("cubek".into(), "abc123".into())],
            seeded: None,
            steps: vec![provenance::StepRecord { lock: "shared", status: 0, seconds: 3 }],
        };
        let line = run.to_json(42);
        assert!(!line.contains('\n'), "a record has to stay one line");
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(v["label"], "r/x");
        assert_eq!(v["revisions"]["cubek"], "abc123");
        assert_eq!(v["steps"][0]["seconds"], 3);
        assert_eq!(v["needs"], "gpu, num_tensor_cores >= 1");
    }
}
