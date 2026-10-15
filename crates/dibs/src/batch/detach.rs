//! `dibs batch --detach`: a batch sent to its machine and driven there, so it outlives this
//! computer, then followed from here to its summary. `docs/design/detach.md` is the design.

use super::{
    base::{Options, machine_of, state_dir},
    detached::{Detached, Followed, Instructions},
    parse::{BatchError, Step, parse, split_words},
};
use crate::{
    call::{CallError, Driver, MachineCall, Origin, Output, Series, Sync},
    caller::Caller,
    cli::{Call, Invocation, Mode, RecipeCall, RecipeVerb},
    execution::SentFrom,
    git::{Git, GitError},
    itself::Itself,
    machine::{
        Client, Delivery, Installed, Liveness, MISSING, Provision, Route, SSH_FAILED, Session, Ssh,
        Stream, Target,
    },
    paths::{FileError, Paths},
    recipe::{Checkouts, RepoError},
    records::RunLog,
};
use dibs_format::{BatchId, Exit, MachineName};
use std::{
    fmt, fs,
    io::Read as _,
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Why a batch cannot be detached, said before anything is sent.
#[derive(Debug)]
pub enum DetachError {
    /// A step names no machine, and a detached batch is driven on the one machine it names.
    Unplaced {
        step: String,
    },
    /// Two steps go to two machines.
    Machines {
        step: String,
        machine: String,
        first: String,
    },
    /// A step does something on this computer, which the machine cannot do for it.
    Here {
        step: String,
        why: &'static str,
    },
    /// A step names a repo by a path, which means nothing on the machine.
    Path {
        step: String,
        repo: String,
    },
    Repo {
        step: String,
        error: RepoError,
    },
    /// A repo named could not be read for sending.
    Git {
        repo: String,
        error: GitError,
    },
    /// `--follow` of a batch this computer did not detach.
    NotDetached(BatchId),
}

impl fmt::Display for DetachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DetachError::Unplaced { step } => write!(
                f,
                "step {step} names no machine, and a detached batch is driven on the machine it runs on.\n  \
                 Give it --on <machine>, or export DIBS_ON=<machine> before the batch to cover every step."
            ),
            DetachError::Machines {
                step,
                machine,
                first,
            } => write!(
                f,
                "step {step} goes to {machine} and an earlier step to {first}: a detached batch is driven on\n  \
                 its one machine. Detach one batch per machine."
            ),
            DetachError::Here { step, why } => write!(
                f,
                "step {step} {why}, and a detached batch runs entirely on its machine"
            ),
            DetachError::Path { step, repo } => write!(
                f,
                "step {step} names {repo} by a path, which means nothing on the machine. Name the repo as\n  \
                 DIBS_ROOT finds it, or run the batch from inside it and name it '.'"
            ),
            DetachError::Repo { step, error } => write!(f, "step {step}: {error}"),
            DetachError::Git { repo, error } => {
                write!(f, "{repo} could not be read to be sent: {error}")
            }
            DetachError::NotDetached(id) => write!(
                f,
                "batch {id} was not detached from this computer, so there is nothing here to follow"
            ),
        }
    }
}

/// A batch's steps, read for the one machine they go to and what they need of this computer.
struct Steps<'a>(&'a [Step]);

impl Steps<'_> {
    fn machine(&self, on: Option<&MachineName>) -> Result<MachineName, BatchError> {
        let mut first: Option<String> = None;
        for step in self.0 {
            let Some(machine) = machine_of(step, on).filter(|m| m != "?") else {
                return Err(DetachError::Unplaced {
                    step: step.name.clone(),
                }
                .into());
            };
            match &first {
                Some(other) if *other != machine => {
                    return Err(DetachError::Machines {
                        step: step.name.clone(),
                        machine,
                        first: other.clone(),
                    }
                    .into());
                }
                Some(_) => {}
                None => first = Some(machine),
            }
        }
        Ok(MachineName::new(first.ok_or(BatchError::NoSteps)?))
    }

    /// Every repo the steps name, each once, refusing a step that acts on this computer.
    fn repos(&self) -> Result<Vec<SentRepo>, BatchError> {
        let checkouts = Checkouts::here().map_err(|error| DetachError::Repo {
            step: String::new(),
            error,
        })?;
        let mut repos: Vec<SentRepo> = Vec::new();
        for step in self.0 {
            for name in Steps::names(step)? {
                if name.contains('/') || (name.starts_with('.') && name != ".") {
                    return Err(DetachError::Path {
                        step: step.name.clone(),
                        repo: name,
                    }
                    .into());
                }
                let dir = checkouts.find(&name).map_err(|error| DetachError::Repo {
                    step: step.name.clone(),
                    error,
                })?;
                // `.` is the repo the batch runs in, which the driver starts in there.
                let name = match name.as_str() {
                    "." => SentRepo::folder(&dir),
                    _ => name,
                };
                if !repos.iter().any(|r| r.name == name) {
                    repos.push(SentRepo { name, dir });
                }
            }
        }
        Ok(repos)
    }

    /// The repos a step names, or why it cannot run detached.
    fn names(step: &Step) -> Result<Vec<String>, BatchError> {
        let refused = |why: &'static str| {
            BatchError::from(DetachError::Here {
                step: step.name.clone(),
                why,
            })
        };
        let words = split_words(&step.line).map_err(|why| BatchError::Line { line: 0, why })?;
        match Invocation::parse_unexpanded(&words[1..]) {
            Ok(Invocation::Call(call)) => match &call.mode {
                Mode::Sync(_) => Err(refused("copies between this computer and the machine")),
                Mode::Run(run) if run.hold => {
                    Err(refused("runs its command on this computer under --hold"))
                }
                Mode::Fetch { .. } => Err(refused("fetches into this computer")),
                Mode::Check { .. } | Mode::Update | Mode::Forget(_) | Mode::Measure { .. } => {
                    Err(refused("changes this computer's dibs"))
                }
                _ => Ok(Vec::new()),
            },
            Ok(Invocation::Recipe(call)) => Steps::recipe_names(&call).map_err(refused),
            _ => Ok(Vec::new()),
        }
    }

    /// The repos a recipe step names: its own and those it pins.
    fn recipe_names(call: &RecipeCall) -> Result<Vec<String>, &'static str> {
        if call.root.is_some() {
            return Err("names --root, a directory on this computer");
        }
        if call.artifacts_to.is_some() {
            return Err("copies its artifacts into a directory on this computer");
        }
        if call.verb == RecipeVerb::With && !call.there {
            return Err("runs its command on this computer; --there runs it on the machine");
        }
        let names_a_repo = matches!(
            call.verb,
            RecipeVerb::Build
                | RecipeVerb::Test
                | RecipeVerb::Bench
                | RecipeVerb::Shell
                | RecipeVerb::With
                | RecipeVerb::List
        );
        if !names_a_repo {
            return Ok(Vec::new());
        }
        let pinned = call
            .pins
            .iter()
            .map(|pin| pin.split_once('@').map_or(pin.as_str(), |(repo, _)| repo));
        Ok(std::iter::once(call.repo.as_str())
            .chain(pinned)
            .map(str::to_string)
            .collect())
    }
}

/// A repo a step names, as this computer finds it: sent whole, `.git` and all, so the driver
/// there resolves its refs and its local tree as this computer would.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SentRepo {
    /// The name it is found by there, under the root the driver is given.
    name: String,
    /// Its top level here.
    dir: PathBuf,
}

impl SentRepo {
    fn folder(dir: &Path) -> String {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into())
    }

    fn git(&self, args: &[&str]) -> Result<String, DetachError> {
        Git(&self.dir)
            .run(args)
            .map(|out| out.trim_end().to_string())
            .map_err(|error| DetachError::Git {
                repo: self.name.clone(),
                error,
            })
    }
}

/// What is sent, laid out here first under the batch's directory, and kept: a follow compares
/// the series file that comes back against the one sent.
struct Staged {
    dir: PathBuf,
}

impl Staged {
    const DIR: &'static str = "send";

    fn of(batch: &Path) -> Result<Staged, FileError> {
        let dir = batch.join(Staged::DIR);
        fs::create_dir_all(&dir).map_err(FileError::at(&dir))?;
        Ok(Staged { dir })
    }

    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), FileError> {
        let path = self.dir.join(name);
        fs::write(&path, bytes).map_err(FileError::at(&path))
    }

    /// The files of `from` with that extension, into `name`, when `from` is there.
    fn copy(&self, from: &Path, name: &str, extension: &str) -> Result<(), FileError> {
        let Ok(entries) = fs::read_dir(from) else {
            return Ok(());
        };
        let to = self.dir.join(name);
        fs::create_dir_all(&to).map_err(FileError::at(&to))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == extension) {
                let into = to.join(entry.file_name());
                fs::copy(&path, &into).map_err(FileError::at(&into))?;
            }
        }
        Ok(())
    }
}

/// A batch on its machine, from this computer.
struct Detach<'a> {
    id: BatchId,
    /// The batch's directory here.
    dir: PathBuf,
    name: MachineName,
    machine: &'a MachineCall<'a>,
    target: Target,
    session: Session,
}

/// Present once what a follow brought back has been filed here, so a second files nothing.
const COLLECTED: &str = "collected";

/// Sends the batch to its machine, starts it there, and follows it to its summary.
pub fn detach(text: &str, opts: &Options) -> Result<i32, BatchError> {
    let parsed = parse(text)?;
    let steps = Steps(&parsed);
    let name = steps.machine(opts.on.as_ref())?;
    let repos = steps.repos()?;
    if opts.dry_run {
        println!(
            "batch (dry run), {} steps, detached to {name}, sending:",
            parsed.len()
        );
        for repo in &repos {
            println!("  {}  from {}", repo.name, repo.dir.display());
        }
        println!("  your recipes and machines");
        return Ok(0);
    }
    let id = BatchId::starting();
    let dir = state_dir().join(id.as_str());
    fs::create_dir_all(&dir).map_err(FileError::at(&dir))?;
    fs::write(dir.join(Driver::DETACHED), name.as_str()).map_err(FileError::at(&dir))?;
    eprintln!(
        "dibs: batch {id}, {} steps, detached to {name}: it is driven there, and carries on whether or not this call does.",
        parsed.len()
    );
    let caller = Caller::from_env();
    let call = Call {
        on: Some(name.clone()),
        ..Call::default()
    };
    let machine = MachineCall::new(&call, &caller)?;
    let detach = Detach::of(id, dir, name, &machine)?;
    if let Some(code) = detach.provision() {
        return Ok(code);
    }
    let instructions = detach.instructions(&caller, &repos);
    if let Some(code) = detach.send(text, &instructions, &repos)? {
        return Ok(code);
    }
    if let Some(code) = detach.start() {
        return Ok(code);
    }
    detach.follow()
}

/// Follows a batch detached from here to its summary, which it prints at once if it has ended.
pub fn follow(id: &BatchId) -> Result<i32, BatchError> {
    let dir = state_dir().join(id.as_str());
    let name = fs::read_to_string(dir.join(Driver::DETACHED))
        .map_err(|_| DetachError::NotDetached(id.clone()))?;
    let name = MachineName::new(name.trim());
    let caller = Caller::from_env();
    let call = Call {
        on: Some(name.clone()),
        ..Call::default()
    };
    let machine = MachineCall::new(&call, &caller)?;
    Detach::of(id.clone(), dir, name, &machine)?.follow()
}

impl<'a> Detach<'a> {
    fn of(
        id: BatchId,
        dir: PathBuf,
        name: MachineName,
        machine: &'a MachineCall<'a>,
    ) -> Result<Detach<'a>, BatchError> {
        let target = machine.target()?;
        let session = Session::new(&target, &machine.here);
        Ok(Detach {
            id,
            dir,
            name,
            machine,
            target,
            session,
        })
    }

    /// The runner and the client of this version there, built first when they are not. Some
    /// exit when they cannot be.
    fn provision(&self) -> Option<i32> {
        if self.session.route == Route::Here {
            return None;
        }
        let provision = Provision {
            session: &self.session,
            live: Liveness::from_env(),
        };
        let mut delivery = Delivery::Inherit;
        let client = match provision.ensure(&mut delivery) {
            Ok(Installed::Done) => provision.client(&mut delivery),
            other => other,
        };
        match client {
            Ok(Installed::Done) => None,
            Ok(Installed::Unreached) => Some(self.session.exit(SSH_FAILED, &self.target)),
            Ok(Installed::NoneThere) => Some(provision.none_there(&mut delivery)),
            Ok(installed @ (Installed::NoLockTaker | Installed::Unlockable)) => {
                Some(provision.unlocked(installed, &mut delivery))
            }
            Ok(Installed::Failed | Installed::Queued { .. }) | Err(_) => {
                eprintln!(
                    "dibs: the dibs that drives a detached batch could not be built on {}, so nothing was sent. Its build said why, above.",
                    self.name
                );
                Some(i32::from(Exit::NoRunner.code()))
            }
        }
    }

    /// What the driver is told: its machine, who sent it, and where it starts, which is inside
    /// the copy of the repo the batch runs in when that was sent, so `.` names it there too, or
    /// else a directory named as this one, which a step naming no label is filed under.
    fn instructions(&self, caller: &Caller, repos: &[SentRepo]) -> Instructions {
        let here = std::env::current_dir().unwrap_or_default();
        let top = Git(&here)
            .run(&["rev-parse", "--show-toplevel"])
            .ok()
            .map(|top| PathBuf::from(top.trim()));
        let cwd = match repos.iter().find(|r| Some(&r.dir) == top.as_ref()) {
            Some(repo) => Path::new(Detached::ROOT).join(&repo.name),
            None => Path::new("here").join(SentRepo::folder(&here)),
        };
        Instructions {
            env: [
                ("DIBS_ON", self.name.to_string()),
                ("DIBS_AGENT", caller.name.clone()),
                ("CLAUDE_CODE_SESSION_ID", caller.id.clone()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
            cwd,
        }
    }

    /// Where on the machine, under its home, what this batch reads is sent.
    fn there(&self, under: &str) -> String {
        format!(":~/{}/{}/{under}", Detached::DIR, self.id)
    }

    /// The batch, what its driver needs to know, and every repo it names. Some exit when a
    /// transfer failed.
    fn send(
        &self,
        text: &str,
        instructions: &Instructions,
        repos: &[SentRepo],
    ) -> Result<Option<i32>, BatchError> {
        let staged = Staged::of(&self.dir)?;
        let paths = Paths::from_env();
        staged.write(Detached::BATCH, text.as_bytes())?;
        let json = serde_json::to_vec(instructions).unwrap_or_default();
        staged.write(Detached::INSTRUCTIONS, &json)?;
        let sent_from: String = repos
            .iter()
            .map(|r| SentFrom::line(&r.name, &r.dir))
            .collect();
        staged.write(Detached::SENT_FROM, sent_from.as_bytes())?;
        let series = paths
            .series()
            .and_then(|path| fs::read(path).ok())
            .unwrap_or_default();
        staged.write(Detached::SERIES, &series)?;
        if let Some(recipes) = paths.recipes() {
            staged.copy(&recipes, Detached::RECIPES, "toml")?;
        }
        if let Some(inventory) = paths.inventory() {
            if let Ok(bytes) = fs::read(&inventory) {
                staged.write(Detached::MACHINES, &bytes)?;
            }
            if let Some(beside) = inventory.parent() {
                staged.copy(&beside.join("machines.d"), "machines.d", "toml")?;
            }
        }
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        eprintln!(
            "dibs: sending {} to {}",
            match names.is_empty() {
                true => "the batch".to_string(),
                false => format!("the batch and {}", names.join(", ")),
            },
            self.name
        );
        let what = format!("{}/", staged.dir.display());
        let into = self.there("");
        if let Some(code) = self.transfer(&["-rl", &what, &into], "the batch")? {
            return Ok(Some(code));
        }
        for repo in repos {
            if let Some(code) = self.send_repo(repo, &staged)? {
                return Ok(Some(code));
            }
        }
        Ok(None)
    }

    /// A repo's files as its ignore rules leave them, then its git directory, each against the
    /// copy the last batch left there. A worktree keeps its own HEAD and index apart from the
    /// repo's, so those go last, over the repo's.
    fn send_repo(&self, repo: &SentRepo, staged: &Staged) -> Result<Option<i32>, BatchError> {
        let listed = repo.git(&["ls-files", "-co", "--exclude-standard", "-z"])?;
        let files: Vec<&str> = listed
            .split('\0')
            .filter(|f| !f.is_empty() && repo.dir.join(f).is_file())
            .collect();
        let list = format!("{}.files", repo.name);
        staged.write(&list, files.join("\0").as_bytes())?;
        let own = PathBuf::from(repo.git(&["rev-parse", "--absolute-git-dir"])?);
        let common = repo
            .dir
            .join(repo.git(&["rev-parse", "--git-common-dir"])?)
            .canonicalize()
            .map_err(FileError::at(&repo.dir))?;
        let root = format!("{}/{}/", Detached::ROOT, repo.name);
        let kept = format!("../../../{}/{}", Detached::LAST, repo.name);
        let tree = [
            "-rlp".to_string(),
            "--checksum".into(),
            "--from0".into(),
            format!("--files-from={}", staged.dir.join(&list).display()),
            format!("--copy-dest={kept}"),
            format!("{}/", repo.dir.display()),
            self.there(&root),
        ];
        if let Some(code) = self.transfer(&tree, &format!("{}'s files", repo.name))? {
            return Ok(Some(code));
        }
        let git_dir = [
            "-rlp".to_string(),
            "--checksum".into(),
            format!("--copy-dest=../{kept}/.git"),
            format!("{}/", common.display()),
            self.there(&format!("{root}.git/")),
        ];
        if let Some(code) = self.transfer(&git_dir, &format!("{}'s git directory", repo.name))? {
            return Ok(Some(code));
        }
        if own == common {
            return Ok(None);
        }
        let worktree = [
            "-lp".to_string(),
            own.join("HEAD").display().to_string(),
            own.join("index").display().to_string(),
            self.there(&format!("{root}.git/")),
        ];
        self.transfer(&worktree, &format!("{}'s worktree head", repo.name))
    }

    /// One rsync to the machine under its shared lock, quiet unless it fails. Some exit then.
    fn transfer<S: AsRef<str>>(&self, args: &[S], what: &str) -> Result<Option<i32>, BatchError> {
        let args: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
        let mut said = Vec::new();
        let mut keep = |_: Stream, line: &[u8]| said.extend_from_slice(line);
        let code = Sync {
            machine: self.machine,
            args: &args,
            origin: Origin::Words,
        }
        .answer_into(&mut Output::Lines(&mut keep))?;
        if code == 0 {
            return Ok(None);
        }
        eprint!("{}", String::from_utf8_lossy(&said));
        eprintln!(
            "dibs: {what} could not be sent to {}, so batch {} was not started",
            self.name, self.id
        );
        Ok(Some(code))
    }

    /// The client there, run with its word, `verb` and the batch.
    fn client(&self, verb: &str) -> Command {
        let words = [Detached::WORD, verb, self.id.as_str()];
        match &self.session.route {
            Route::Here => {
                let mut command = Itself::command();
                command.args(words);
                command
            }
            Route::Ssh { host, config } => {
                let mut ssh = Ssh::to(config.as_deref());
                ssh.args(Ssh::options())
                    .arg(host)
                    .arg(Client::far_line(&words.join(" ")));
                // SAFETY: the closure makes async-signal-safe calls only.
                unsafe {
                    ssh.pre_exec(|| {
                        Ssh::parent_death_signal();
                        Ok(())
                    });
                }
                ssh
            }
        }
    }

    /// Starts the driver there. Some exit when it did not start.
    fn start(&self) -> Option<i32> {
        let started = self
            .client("start")
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output();
        let out = match started {
            Ok(out) => out,
            Err(e) => {
                eprintln!("dibs: batch {} could not be started: {e}", self.id);
                return Some(Exit::Failed.status());
            }
        };
        match Exit::shell_status(out.status) {
            0 => {
                eprintln!(
                    "dibs: batch {} started on {}, {}. This call follows it to its summary.",
                    self.id,
                    self.name,
                    String::from_utf8_lossy(&out.stdout).trim()
                );
                None
            }
            MISSING => {
                eprintln!(
                    "dibs: {} has no dibs of this version to drive the batch, so it was not started",
                    self.name
                );
                Some(i32::from(Exit::NoRunner.code()))
            }
            code => Some(self.session.exit(code, &self.target)),
        }
    }

    /// Waits there for the batch to end, then files here what it left and prints its summary.
    fn follow(&self) -> Result<i32, BatchError> {
        let mut child = self
            .client("follow")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(CallError::Io)?;
        let mut json = String::new();
        if let Some(mut out) = child.stdout.take() {
            let _ = out.read_to_string(&mut json);
        }
        let status = child
            .wait()
            .map(Exit::shell_status)
            .unwrap_or(Exit::Failed.status());
        if status == SSH_FAILED {
            eprintln!(
                "dibs: lost {} while following batch {}, which carries on there. Its summary, once it has ended:\n    dibs batch --follow {}",
                self.name, self.id, self.id
            );
            return Ok(i32::from(Exit::Unreachable.code()));
        }
        match serde_json::from_str::<Followed>(json.trim()) {
            Ok(followed) if status == 0 => Ok(self.collect(followed)),
            _ => {
                eprintln!(
                    "dibs: batch {} could not be followed on {} (exit {status})",
                    self.id, self.name
                );
                Ok(status.max(1))
            }
        }
    }

    /// Files here what the batch left there: each step's output beside the batch, and its run
    /// records and series with this computer's, once.
    fn collect(&self, followed: Followed) -> i32 {
        for (name, text) in &followed.files {
            let _ = fs::write(self.dir.join(name), text);
        }
        if !self.dir.join(COLLECTED).exists() {
            if let Ok(runs) = RunLog::here() {
                let _ = runs.append_lines(&followed.runs);
            }
            if let Some(path) = Paths::from_env().series() {
                let sent = fs::read_to_string(self.dir.join(Staged::DIR).join(Detached::SERIES))
                    .unwrap_or_default();
                Series { path }.merge(&sent, &followed.series);
            }
            let _ = fs::write(self.dir.join(COLLECTED), "");
        }
        let Some(summary) = followed.summary else {
            eprint!("{}", followed.driver);
            eprintln!(
                "dibs: batch {} ended on {} without a summary; what its driver said is above",
                self.id, self.name
            );
            return Exit::Failed.status();
        };
        let summary = summary.replace(&followed.dir, &self.dir.display().to_string());
        let _ = fs::write(self.dir.join("summary"), &summary);
        print!("{summary}");
        println!(
            "its jobs ran on {}:  dibs --on {} --out <job>",
            self.name, self.name
        );
        followed.exit.unwrap_or(Exit::Failed.status())
    }
}
