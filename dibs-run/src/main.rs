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

mod provenance;
mod recipe;
mod resource;
mod runs;
mod worktree;

use recipe::{Lock, Manifest, Verb};
use resource::{Backend, Dibs, Request};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
dibs-run <verb> <repo>[@<ref>] <recipe>   run a recipe from the repo's .dibs.toml
dibs-run list <repo>                      what that repo defines
dibs-run runs [label]                     what has run here, and what is comparable
dibs-run shell <repo>[@<ref>] --reason <why> -- <cmd>   a command in a prepared worktree
dibs-run raw --reason <why> -- <cmd>      a command with nothing prepared
dibs-run gaps                             what did not fit a recipe, and what recurs

  <verb>    bench, build or test
  <repo>    a path to a checkout, or a name resolved under --root
  --root    where named repos live (default $DIBS_ROOT, else the current directory)
  --reason  why this does not fit a recipe. Required for shell and raw, and recorded:
            a reason that keeps recurring is the specification for the next recipe.
  --device  the card to run on, named from the machine's inventory. It is part of the
            derived label, so each card keeps its own history and running a recipe on a
            second one neither mixes with the first nor replaces it. `dibs --machines -v`
            lists the aliases.
  --dry-run print what would run, take no lock, record nothing

A recipe declares the procedure and names no revisions: the invocation supplies the code and
the run record captures what it resolved to.

Which machine comes from DIBS_HOST, the same as it does for dibs itself.
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("dibs-run: {e}");
            ExitCode::from(2)
        }
    }
}

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
}

fn parse() -> Result<Args, String> {
    let mut positional: Vec<String> = Vec::new();
    let mut root = repo_root();
    let mut dry_run = false;
    let mut reason = None;
    let mut command = None;
    let mut device: Option<String> = None;
    let mut it = std::env::args().skip(1);
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
            "--root" => root = PathBuf::from(it.next().ok_or("--root needs a path")?),
            "--dry-run" => dry_run = true,
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
    let (repo, reference) = match target.split_once('@') {
        Some((r, rev)) => (r.to_string(), Some(rev.to_string())),
        None => (target, None),
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
    })
}

/// Where a bare repo name is looked up. Everyone lays their checkouts out differently, so
/// this is only a starting guess: DIBS_ROOT, then --root, then the directory you are in.
fn repo_root() -> PathBuf {
    match std::env::var_os("DIBS_ROOT") {
        Some(r) => PathBuf::from(r),
        None => PathBuf::from("."),
    }
}

fn run() -> Result<ExitCode, String> {
    let args = parse()?;

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
            backend: backend.name(),
            device: args.device.clone(),
            machine: backend.machine.clone(),
            revisions: Vec::new(),
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

    let dir = resolve_repo(&args.repo, &args.root)?;
    let repo_name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_string();
    let manifest = if args.verb == "shell" {
        Manifest::default()
    } else {
        Manifest::load(&dir, &repo_name)?
    };

    if args.verb == "list" {
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
                }
            }
        }
        println!("\nlocal recipes: {}", recipe::local_dir().display());
        return Ok(ExitCode::SUCCESS);
    }

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
        steps: vec![recipe::Step {
            lock: Lock::Shared,
            run: args.command.clone().unwrap_or_default(),
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
    let rec = shell_recipe.as_ref().map(Ok).unwrap_or_else(|| manifest.recipe(verb, name).ok_or_else(|| {
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
    let fingerprint = rec.fingerprint();
    // The duration history keys on lock and label together, so a recipe's build and its
    // measurement stay apart on their own. Two steps taking the *same* lock would not, and
    // their durations would average into one meaningless number: the bimodal history that
    // made estimates useless in the first place, rebuilt deliberately.
    let step_labels = label_steps(&label, &rec.steps);

    if args.dry_run {
        println!("label       {label}");
        println!("recipe      {name}  ({fingerprint})");
        println!("isolation   {:?}", rec.isolation);
        if let Some(n) = &rec.needs {
            println!("needs       {n}");
        }
        println!("ref         {}", args.reference.as_deref().unwrap_or("HEAD"));
        // Which card, printed whether or not one was named: a dry run is where someone checks
        // they are about to measure the thing they mean to, and "no card named" is the answer
        // that most needs saying, because that run is the one nobody can repeat.
        match &args.device {
            Some(d) => println!("device      {d}"),
            None => println!("device      none named, so the runtime picks and a repeat is luck"),
        }
        for (i, s) in rec.steps.iter().enumerate() {
            println!("step {}      [{:?}] {}", i + 1, s.lock, s.run);
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
    eprintln!("dibs-run: preparing {repo_name}@{reference}");
    let setup = Request {
        label: &format!("{label}:setup"),
        lock: Lock::Shared,
        isolation: rec.isolation,
        needs: None,
        // Preparing a worktree touches no GPU, so pinning it would only make the setup fail
        // on a machine whose card has been pulled.
        device: None,
        };
    let (out, text) = backend.run_capture(&setup, &worktree::setup_script(&repo_name, reference))?;
    if out.status != 0 {
        return Err(format!("could not prepare {repo_name}@{reference} (exit {})", out.status));
    }
    let prepared = worktree::parse(&text)?;
    eprintln!("dibs-run: {}", prepared.worktree);

    let mut steps = Vec::new();
    let mut failed = None;

    for (i, step) in rec.steps.iter().enumerate() {
        // The step says which lock it wants, where it can be reviewed, instead of a compile
        // being invisible inside a script that holds the machine exclusively.
        let req = Request {
            label: &step_labels[i],
            lock: step.lock,
            isolation: rec.isolation,
            needs: rec.needs.as_deref(),
            device: args.device.as_deref(),
        };
        // One cache per repo, exported rather than left to each recipe to remember, and the
        // output always lands in a file. That second part is not tidiness: `dibs --out` reads
        // a running job by finding the file it redirected into, and in the log this replaces,
        // two jobs out of 179 redirected. A feature that worked one per cent of the time now
        // works every time, because nothing is being asked to remember.
        //
        // stderr is merged rather than teed separately. Keeping them apart needs a process
        // substitution per stream, and for a build log the merge is what everyone wants
        // anyway. pipefail so the step's exit status survives the pipe into tee.
        let log = format!("{}/out/{}.log", prepared.scratch, step_labels[i].replace('/', "-"));
        let cd = format!(
            "cd {} && export CARGO_TARGET_DIR={} && set -o pipefail && {{ {}; }} 2>&1 | tee {}",
            sh(&prepared.worktree),
            sh(&prepared.target),
            step.run,
            sh(&log)
        );
        eprintln!("dibs-run: step {}/{} [{:?}]", i + 1, rec.steps.len(), step.lock);
        let out = backend.run(&req, &cd)?;
        steps.push(provenance::StepRecord {
            lock: match step.lock {
                Lock::Shared => "shared",
                Lock::Exclusive => "exclusive",
            },
            status: out.status,
            seconds: out.seconds,
        });
        if out.status != 0 {
            failed = Some(out.status);
            break;
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
            .map(|st| (format!("{:?}", st.lock).to_lowercase(), st.run.clone()))
            .collect(),
        backend: backend.name(),
        device: args.device.clone(),
        machine: backend.machine.clone(),
        // Read on the machine, from the tree that was actually built, rather than from a
        // checkout here that may be at a different commit entirely.
        revisions: prepared.revisions.clone(),
        steps,
    };
    write_record(&record)?;

    Ok(match failed {
        Some(c) => ExitCode::from(c.clamp(1, 255) as u8),
        None => ExitCode::SUCCESS,
    })
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
        "no repo at '{repo}' and none under {}; give a path or set --root",
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
        Step { lock, run: run.into() }
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
            steps: vec![step(Lock::Shared, "cargo build")],
        };
        let same = Recipe {
            source: recipe::Source::Repo,
            needs: None,
            isolation: Isolation::Machine,
            steps: vec![step(Lock::Shared, "cargo build")],
        };
        let changed_command = Recipe {
            source: recipe::Source::Repo,
            needs: None,
            isolation: Isolation::Machine,
            steps: vec![step(Lock::Shared, "cargo build --release")],
        };
        let changed_lock = Recipe {
            source: recipe::Source::Repo,
            needs: None,
            isolation: Isolation::Machine,
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
            backend: "dibs",
            device: Some("gpu:rtx2060".into()),
            machine: Some("multigpu".into()),
            revisions: vec![],
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
            backend: "dibs",
            device: None,
            machine: None,
            revisions: vec![],
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
            backend: "dibs",
            device: None,
            machine: None,
            revisions: vec![("cubek".into(), "abc123".into())],
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
