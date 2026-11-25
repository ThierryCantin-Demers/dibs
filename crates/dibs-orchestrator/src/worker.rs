//! A submission's worker: it holds the submission, makes each planned step's dibs calls with the
//! orchestrator's own dibs, and records how each went. It outlives the API call that started it,
//! and a worker started after it (on resume, or after a restore) carries on from the last step
//! the store records, asking the machine how an attempt it never heard back from went.

use crate::{
    home::Home,
    spawn::{self, DRIVEN, DRIVING},
    split::{Resolved, shell_word},
    store::{Attempt, Fence, Settled, Step, Store},
};
use dibs_format::{
    By, Exit, Sha256,
    orchestrator::{NamedCall, Seal, State, Submission, Work},
    wire::{Admitted, Built, Bundle, BundleCheck, BundleMake, BundleManifest, Record, Trailer},
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead as _, BufReader, Write as _},
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// Set by a signal: the submission is being killed, or this worker stopped.
static STOP: AtomicBool = AtomicBool::new(false);

/// How often a worker waiting on its call looks whether it was told to stop.
const TICK: Duration = Duration::from_millis(200);
/// How long a worker waits before asking again about an attempt whose job still runs, unless
/// `DIBS_ORCH_RECHECK` gives seconds.
fn still_running() -> Duration {
    let seconds = std::env::var("DIBS_ORCH_RECHECK")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    Duration::from_secs(seconds)
}
/// Attempts at one idempotent step before it is failed.
const ATTEMPTS: i64 = 3;
/// How long a worker waits before asking again about an attempt whose call heard nothing back.
const UNHEARD: Duration = Duration::from_secs(5);
/// How long a worker waits before asking again a machine it could not reach.
const UNREACHABLE: Duration = Duration::from_secs(60);

pub fn lock_path(home: &Home, id: &str) -> PathBuf {
    home.submission(id).join("worker.lock")
}

/// Whether a live worker holds the submission.
pub fn driven(home: &Home, id: &str) -> bool {
    File::open(lock_path(home, id))
        .is_ok_and(|f| matches!(f.try_lock_shared(), Err(fs::TryLockError::WouldBlock)))
}

pub fn step_log(home: &Home, id: &str, n: i64, name: &str) -> PathBuf {
    home.submission(id)
        .join("steps")
        .join(format!("{n}-{name}.log"))
}

/// The orchestrator's own dibs: `DIBS_ORCH_DIBS`, else the one beside this binary.
pub fn dibs() -> Command {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("dibs")))
        .filter(|p| p.is_file());
    let program = std::env::var_os("DIBS_ORCH_DIBS")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or(beside)
        .unwrap_or_else(|| PathBuf::from("dibs"));
    Command::new(program)
}

pub fn run(home: &Home, id: &str) -> i32 {
    let fifo = spawn::started_fifo(home, id);
    let dir = home.submission(id);
    let _ = fs::create_dir_all(dir.join("steps"));
    let held = match File::create(lock_path(home, id)).map(|f| (f.try_lock(), f)) {
        Ok((Ok(()), f)) => f,
        Ok((Err(fs::TryLockError::WouldBlock), _)) => {
            spawn::tell(&fifo, DRIVEN);
            eprintln!("dibs-orchestrator: another worker drives {id}");
            return 0;
        }
        Ok((Err(fs::TryLockError::Error(e)), _)) | Err(e) => {
            eprintln!("dibs-orchestrator: {id}'s lock: {e}");
            return Exit::Failed.status();
        }
    };
    let store = match Store::open(&home.store()) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("dibs-orchestrator: the store: {e}");
            return Exit::Failed.status();
        }
    };
    let (coworker, submission) = match store.submission(id) {
        Ok(Some(found)) => found,
        other => {
            eprintln!("dibs-orchestrator: no submission {id} to drive ({other:?})");
            return Exit::Refused.status();
        }
    };
    listen_for_stop();
    let fence = match store.claim(id, std::process::id()) {
        Ok(fence) => fence,
        Err(e) => {
            eprintln!("dibs-orchestrator: {id} could not be claimed: {e}");
            return Exit::Failed.status();
        }
    };
    spawn::tell(&fifo, DRIVING);
    let resolved = store
        .resolved(id)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<Resolved>(&text).ok());
    let worker = Worker {
        home,
        store: &store,
        id,
        coworker: &coworker,
        submission: &submission,
        resolved: resolved.as_ref(),
        fence,
    };
    let code = worker.drive();
    drop(held);
    code
}

struct Worker<'a> {
    home: &'a Home,
    store: &'a Store,
    id: &'a str,
    coworker: &'a str,
    submission: &'a Submission,
    resolved: Option<&'a Resolved>,
    fence: Fence,
}

/// How a step ended, and the attempt it closes with: id and what was heard of it.
struct Ended {
    state: State,
    exit: i32,
    trailer: Option<Trailer>,
    note: Option<String>,
    attempt: Option<(String, String)>,
}

impl Ended {
    fn done(trailer: Option<Trailer>, note: Option<String>) -> Ended {
        Ended {
            state: State::Done,
            exit: 0,
            trailer,
            note,
            attempt: None,
        }
    }

    fn failed(exit: i32, note: impl Into<String>) -> Ended {
        Ended {
            state: State::Failed,
            exit,
            trailer: None,
            note: Some(note.into()),
            attempt: None,
        }
    }

    fn killed(trailer: Option<Trailer>, note: &str) -> Ended {
        Ended {
            state: State::Cancelled,
            exit: Exit::Cancelled.status(),
            trailer,
            note: Some(note.into()),
            attempt: None,
        }
    }

    fn of(self, attempt: &str, heard: &str) -> Ended {
        Ended {
            attempt: Some((attempt.to_string(), heard.to_string())),
            ..self
        }
    }
}

/// Whether a step may run again from the start after an attempt whose end was not heard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Builds, transfers and checks.
    Again,
    /// A measurement, never run twice for one submission on its own.
    Once,
}

/// This worker lets go of the submission without recording an end: another worker holds it
/// now, or this one was stopped without the submission being killed, as at a shutdown, and the
/// next to resume it carries on.
struct Leave;

impl Worker<'_> {
    fn drive(&self) -> i32 {
        let steps = match self.store.steps(self.id) {
            Ok(steps) => steps,
            Err(e) => return self.finish(State::Failed, 1, Some(&format!("the store: {e}"))),
        };
        let mut last = Ended::done(None, None);
        for step in &steps {
            if step.state == State::Done {
                continue;
            }
            if step.state.ended() {
                let exit = step.exit.unwrap_or(match step.state {
                    State::Cancelled => Exit::Cancelled.status(),
                    _ => Exit::Failed.status(),
                });
                return self.finish(step.state, exit, None);
            }
            let open = self
                .store
                .last_attempt(self.id, step.n)
                .ok()
                .flatten()
                .is_some_and(|a| a.open);
            if self.store.cancelled(self.id).unwrap_or(false) && !open {
                return self.cancelled(step);
            }
            if STOP.load(Ordering::SeqCst) {
                return 0;
            }
            let ended = match self.step(step) {
                Ok(ended) => ended,
                Err(Leave) => {
                    eprintln!(
                        "dibs-orchestrator: this worker lets go of {}, for the next to carry on",
                        self.id
                    );
                    return 0;
                }
            };
            let settled = self.store.settle(
                self.id,
                step.n,
                ended
                    .attempt
                    .as_ref()
                    .map(|(id, heard)| (id.as_str(), heard.as_str())),
                Settled {
                    state: ended.state,
                    exit: outcome(ended.state, ended.exit),
                    trailer: ended.trailer.as_ref(),
                    note: ended.note.as_deref(),
                },
            );
            if let Err(e) = settled {
                // Left as it was, so the next worker asks about the attempt again.
                eprintln!(
                    "dibs-orchestrator: {}'s step {} could not be recorded: {e}",
                    self.id, step.name
                );
                return Exit::Failed.status();
            }
            if ended.state != State::Done {
                return self.finish(ended.state, ended.exit, ended.note.as_deref());
            }
            last = ended;
        }
        self.finish(State::Done, last.exit, None)
    }

    /// Killed, a step only asks after the attempt it left, before reading anything it would run.
    fn killed(&self) -> bool {
        self.store.cancelled(self.id).unwrap_or(false)
    }

    /// Leave when told to stop without being killed, as at a shutdown: what was cut short is
    /// done again by the next worker.
    fn leaving(&self) -> Result<(), Leave> {
        match STOP.load(Ordering::SeqCst) && !self.store.cancelled(self.id).unwrap_or(false) {
            true => Err(Leave),
            false => Ok(()),
        }
    }

    fn cancelled(&self, step: &Step) -> i32 {
        let _ = self.store.settle(
            self.id,
            step.n,
            None,
            Settled {
                state: State::Cancelled,
                exit: Exit::Cancelled.status(),
                trailer: None,
                note: Some("killed"),
            },
        );
        self.finish(
            State::Cancelled,
            Exit::Cancelled.status(),
            Some("killed with dibs orch kill"),
        )
    }

    fn finish(&self, state: State, exit: i32, note: Option<&str>) -> i32 {
        let exit = outcome(state, exit);
        if let Err(e) = self.store.finish(self.id, state, Some(exit), note) {
            eprintln!("dibs-orchestrator: {} could not be recorded: {e}", self.id);
        }
        exit
    }

    fn step(&self, step: &Step) -> Result<Ended, Leave> {
        match (&self.submission.work, step.name.as_str(), self.resolved) {
            (Work::Call(call), "call", _) => self.call(step, call),
            (Work::Split(_), "build", Some(split)) => self.build(step, split),
            (Work::Split(_), "relay", Some(split)) => self.relay(step, split),
            (Work::Split(_), "stage", Some(split)) => self.stage(step, split),
            (Work::Split(_), "measure", Some(split)) => self.measure(step, split),
            _ => Ok(Ended::failed(
                Exit::Refused.status(),
                format!("this worker does not know the step {}", step.name),
            )),
        }
    }

    fn call(&self, step: &Step, call: &NamedCall) -> Result<Ended, Leave> {
        let mut args = vec!["run".to_string(), "--on".into(), call.on.clone()];
        if call.bench {
            args.push("--bench".into());
        }
        if let Some(device) = &call.device {
            args.extend(["--device".into(), device.clone()]);
        }
        args.extend(["--label".into(), call.label.clone()]);
        if let Some(max) = call.max {
            args.extend(["--max".into(), max.to_string()]);
        }
        args.push(call.command.clone());
        let kind = match call.bench {
            true => Kind::Once,
            false => Kind::Again,
        };
        self.attempted(step, &call.on, &args, &[], None, kind)
    }

    /// The repo's tree as it was sealed, moved out of staging the first time it is needed.
    fn root(&self, split: &Resolved) -> Result<PathBuf, String> {
        let root = self.home.submission(self.id).join("root");
        let tree = root.join(&split.repo);
        let upload = self
            .submission
            .upload
            .as_ref()
            .ok_or("the submission sent no tree")?;
        if !tree.is_dir() {
            let staging = self.home.staging(self.coworker);
            let stage = staging.join("stage").join(&upload.stage);
            fs::create_dir_all(&root).map_err(|e| e.to_string())?;
            let _held = self
                .home
                .stage_lock(self.coworker, &upload.stage)
                .map_err(|e| format!("the stage's lock: {e}"))?;
            fs::rename(&stage, &tree)
                .map_err(|e| format!("the sent tree {} cannot be taken: {e}", stage.display()))?;
            // What the next upload of this repo is sent against, so only what changed travels.
            let last = staging.join("last").join(&split.repo);
            let next =
                staging
                    .join("last")
                    .join(format!(".{}.next-{}", split.repo, std::process::id()));
            if let Some(parent) = last.parent() {
                let _ = fs::create_dir_all(parent);
                let ours = format!(".{}.next-", split.repo);
                for left in fs::read_dir(parent).into_iter().flatten().flatten() {
                    let name = left.file_name().to_string_lossy().into_owned();
                    let pid = name.strip_prefix(&ours).and_then(|pid| pid.parse().ok());
                    if pid.is_some_and(|pid| pid == std::process::id() || !alive(pid)) {
                        let _ = fs::remove_dir_all(left.path());
                    }
                }
            }
            let copied = Command::new("cp").arg("-al").arg(&tree).arg(&next).status();
            // An upload may make an empty one meanwhile, which a rename replaces whole.
            let placed = copied.is_ok_and(|s| s.success()) && {
                let _ = fs::remove_dir_all(&last);
                fs::rename(&next, &last).is_ok()
            };
            if !placed {
                let _ = fs::remove_dir_all(&next);
            }
        }
        // The stage can be sent to again under the same key until it is taken, so what was
        // taken is checked here, its git directory made the service's own again first.
        crate::api::own_git(&tree.join(".git"))
            .map_err(|e| format!("the taken repo's git directory cannot be taken over: {e}"))?;
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&tree)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        if head.as_deref() != Some(upload.head.as_str()) {
            return Err(format!(
                "the taken repo's HEAD is {}, not the accepted {}",
                head.as_deref().unwrap_or("unreadable"),
                upload.head
            ));
        }
        let listed = Seal::listed(&tree).map_err(|e| e.to_string())?;
        let (digest, files) =
            Seal::of(&tree, listed.iter().map(String::as_str)).map_err(|e| e.to_string())?;
        if digest != upload.digest || files != upload.files {
            return Err(format!(
                "the tree taken is not the one accepted ({files} files sealed {digest:.12}, accepted {} sealed {:.12})",
                upload.files, upload.digest
            ));
        }
        Ok(root)
    }

    fn build(&self, step: &Step, split: &Resolved) -> Result<Ended, Leave> {
        if self.killed() {
            return self.attempted(step, &split.compile, &[], &[], None, Kind::Again);
        }
        let root = match self.root(split) {
            Ok(root) => root,
            Err(why) => return Ok(Ended::failed(Exit::Setup.status(), why)),
        };
        // The compile machine keys the tree by coworker and repo, so their builds take turns.
        let lock = self.home.tree_lock(self.coworker, &split.repo);
        if let Some(dir) = lock.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _turn = File::create(&lock).and_then(|f| f.lock().map(|()| f));
        let sent_from = self.home.submission(self.id).join("sent-from");
        let _ = fs::write(
            &sent_from,
            format!(
                "{}\torchestrator/{}/{}\n",
                split.repo, self.coworker, split.repo
            ),
        );
        let bundle = Bundle::Make(BundleMake {
            cargo: split.cargo.clone(),
            package: split.package.clone(),
            kind: split.kind.clone(),
            name: split.name.clone(),
            into: self.id.to_string(),
            env: BTreeMap::from([("RUSTFLAGS".to_string(), split.rustflags.clone())]),
            inputs: BTreeMap::from([
                ("profile".to_string(), split.profile.clone()),
                ("submission".to_string(), self.id.to_string()),
            ])
            .into_iter()
            .chain(
                split
                    .image
                    .clone()
                    .map(|image| ("image".to_string(), image)),
            )
            .collect(),
        });
        let args = vec![
            "shell".to_string(),
            format!("{}@local", split.repo),
            "--on".into(),
            split.compile.clone(),
            "--label".into(),
            format!("{}-build", split.label),
            "--reason".into(),
            "an orchestrator split's build".into(),
            "--preemptible".into(),
            "--".into(),
            split.build_command(),
        ];
        let mut env = vec![
            ("DIBS_ROOT", root.display().to_string()),
            ("DIBS_SENT_FROM", sent_from.display().to_string()),
        ];
        if let Some(image) = &split.image {
            env.push(("DIBS_IMAGE", image.clone()));
        }
        let cwd = root.join(&split.repo);
        self.attempted(
            step,
            &split.compile,
            &args,
            &env,
            Some((&cwd, bundle)),
            Kind::Again,
        )
    }

    /// The bundle from the compile machine into the store, checked and attested, then on to the
    /// GPU machine.
    fn relay(&self, step: &Step, split: &Resolved) -> Result<Ended, Leave> {
        let log = step_log(self.home, self.id, step.n, &step.name);
        let fetched = self.home.submission(self.id).join("bundle");
        let _ = fs::remove_dir_all(&fetched);
        let from = format!(":$DIBS_SCRATCH/{}/{}/", BundleManifest::DIR, self.id);
        let code = self.dibs_logged(
            &log,
            &[
                "--on".into(),
                split.compile.clone(),
                "--sync".into(),
                "-a".into(),
                from,
                format!("{}/", fetched.display()),
            ],
            &[],
            None,
        );
        if code != 0 {
            self.leaving()?;
            return Ok(Ended::failed(
                code,
                format!("the bundle could not be fetched from {}", split.compile),
            ));
        }
        let manifest: BundleManifest = match fs::read(fetched.join(BundleManifest::FILE))
            .ok()
            .and_then(|t| serde_json::from_slice(&t).ok())
        {
            Some(m) => m,
            None => {
                return Ok(Ended::failed(
                    Exit::Setup.status(),
                    "the bundle came without a manifest",
                ));
            }
        };
        let digest = Sha256::hex_of_file(&fetched.join(&manifest.exe)).unwrap_or_default();
        if digest != manifest.digest {
            return Ok(Ended::failed(
                Exit::TargetRebuilt.status(),
                format!(
                    "the bundle's {} is {digest}, not the {} its manifest names",
                    manifest.exe, manifest.digest
                ),
            ));
        }
        let built = self.trailer_of("build").and_then(|t| t.built);
        let inputs = self.inputs(&manifest);
        let attested = self.store.attested(&digest, &inputs).unwrap_or(false);
        if built == Some(Built::Nothing) && !attested {
            return Ok(Ended::failed(
                Exit::TargetRebuilt.status(),
                "the build compiled nothing, and no earlier build of these exact inputs attests the executable it left",
            ));
        }
        let producer = self
            .trailer_of("build")
            .and_then(|t| serde_json::to_string(&t).ok())
            .unwrap_or_default();
        let _ = self.store.attest(
            &digest,
            &inputs,
            &serde_json::to_string(&manifest).unwrap_or_default(),
            &producer,
        );
        let kept = self.home.bundle(&digest);
        if !kept.is_dir() {
            if let Some(parent) = kept.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = fs::rename(&fetched, &kept) {
                return Ok(Ended::failed(
                    Exit::Failed.status(),
                    format!("the bundle could not be kept: {e}"),
                ));
            }
        }
        let _ = fs::write(self.home.submission(self.id).join("digest"), &digest);
        let code = self.dibs_logged(
            &log,
            &[
                "--on".into(),
                split.gpu.clone(),
                "--sync".into(),
                "-a".into(),
                format!("{}/", kept.display()),
                format!(":$DIBS_SCRATCH/{}/{digest}/", BundleManifest::DIR),
            ],
            &[],
            None,
        );
        if code != 0 {
            self.leaving()?;
            return Ok(Ended::failed(
                code,
                format!("the bundle could not be sent to {}", split.gpu),
            ));
        }
        Ok(Ended::done(None, Some(format!("bundle {digest}"))))
    }

    /// What a bundle is bound to: the tree sent, the lockfile, the toolchain, the flags and the
    /// target, so two builds of the same inputs attest each other.
    fn inputs(&self, manifest: &BundleManifest) -> String {
        let upload = self.submission.upload.as_ref();
        let mut bound = serde_json::json!({
            "head": upload.map(|u| u.head.clone()),
            "tree": upload.map(|u| u.digest.clone()),
            "lockfile": manifest.lockfile,
            "rustc": manifest.rustc,
            "rustflags": manifest.rustflags,
            "cargo": manifest.cargo,
            "package": manifest.package,
            "kind": manifest.kind,
            "name": manifest.name,
            "profile": manifest.inputs.get("profile"),
        });
        // Only when there is one, so what a native build attested before still matches.
        if let Some(image) = manifest.inputs.get("image") {
            bound["image"] = serde_json::json!(image);
        }
        Sha256::hex(bound.to_string().as_bytes())
    }

    fn digest(&self) -> Option<String> {
        fs::read_to_string(self.home.submission(self.id).join("digest"))
            .ok()
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
    }

    fn check(&self) -> Option<Bundle> {
        self.digest().map(|digest| {
            Bundle::Check(BundleCheck {
                name: digest.clone(),
                digest,
            })
        })
    }

    /// Shared, on the card: the bundle is there unchanged, and the machine has what it links.
    fn stage(&self, step: &Step, split: &Resolved) -> Result<Ended, Leave> {
        if self.killed() {
            return self.attempted(step, &split.gpu, &[], &[], None, Kind::Again);
        }
        let Some(check) = self.check() else {
            return Ok(Ended::failed(Exit::Setup.status(), "no bundle was relayed"));
        };
        let digest = self.digest().unwrap_or_default();
        let links = match is_static(&self.home.bundle(&digest).join(&split.name)) {
            true => "echo 'static: links nothing'; ".to_string(),
            false => "if LC_ALL=C ldd \"$e\" > \"$TMPDIR/ldd.$$\" 2>&1; then cat \"$TMPDIR/ldd.$$\"; \
                 if grep -q 'not found' \"$TMPDIR/ldd.$$\"; then echo 'dibs: this machine lacks what the bundle links' >&2; rm -f \"$TMPDIR/ldd.$$\"; exit 3; fi; \
                 else cat \"$TMPDIR/ldd.$$\" >&2; echo 'dibs: ldd could not read what the bundle links' >&2; rm -f \"$TMPDIR/ldd.$$\"; exit 3; fi; \
                 rm -f \"$TMPDIR/ldd.$$\"; "
                .to_string(),
        };
        let command = format!(
            "e=\"$DIBS_BUNDLE/{}\"; test -x \"$e\" || {{ echo \"dibs: the bundle has no executable $e\" >&2; exit 3; }}; \
             {links}ldd --version 2>&1 | head -n 1; \
             nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>/dev/null || true",
            split.name
        );
        let args = vec![
            "run".to_string(),
            "--on".into(),
            split.gpu.clone(),
            "--device".into(),
            split.device.clone(),
            "--label".into(),
            format!("{}-stage", split.label),
            command,
        ];
        self.attempted(
            step,
            &split.gpu,
            &args,
            &[],
            Some((Path::new(""), check)),
            Kind::Again,
        )
    }

    fn measure(&self, step: &Step, split: &Resolved) -> Result<Ended, Leave> {
        if self.killed() {
            return self.attempted(step, &split.gpu, &[], &[], None, Kind::Once);
        }
        let Some(check) = self.check() else {
            return Ok(Ended::failed(Exit::Setup.status(), "no bundle was relayed"));
        };
        let exports: String = std::iter::once(("DIBS_SUBMISSION".to_string(), self.id.to_string()))
            .chain(split.env.clone())
            .map(|(k, v)| format!("export {k}=\"{}\"; ", v.replace(['"', '\\', '`'], "")))
            .collect();
        let args = vec![
            "run".to_string(),
            "--bench".into(),
            "--on".into(),
            split.gpu.clone(),
            "--device".into(),
            split.device.clone(),
            "--label".into(),
            split.label.clone(),
            format!("{exports}{}", split.command),
        ];
        self.attempted(
            step,
            &split.gpu,
            &args,
            &[],
            Some((Path::new(""), check)),
            Kind::Once,
        )
    }

    fn trailer_of(&self, name: &str) -> Option<Trailer> {
        self.store
            .detail(self.id)
            .ok()
            .flatten()?
            .steps
            .into_iter()
            .find(|s| s.name == name)?
            .trailer
    }

    /// The step's call, made as an attempt the machine admits once, until it ends in a way that
    /// says how the step went.
    fn attempted(
        &self,
        step: &Step,
        machine: &str,
        args: &[String],
        env: &[(&str, String)],
        bundle: Option<(&Path, Bundle)>,
        kind: Kind,
    ) -> Result<Ended, Leave> {
        let log = step_log(self.home, self.id, step.n, &step.name);
        let (cwd, bundle) = match bundle {
            Some((cwd, bundle)) => (
                Some(cwd).filter(|c| !c.as_os_str().is_empty()),
                serde_json::to_string(&bundle).ok(),
            ),
            None => (None, None),
        };
        let mut unheard = 0;
        loop {
            let killed = self.store.cancelled(self.id).unwrap_or(false);
            if STOP.load(Ordering::SeqCst) && !killed {
                return Err(Leave);
            }
            let attempt = match self.store.last_attempt(self.id, step.n).ok().flatten() {
                Some(open) if open.open => open,
                // Killed: an attempt still open is followed until its job is seen to end, and
                // no new one is made.
                _ if killed => return Ok(Ended::killed(None, "killed with dibs orch kill")),
                Some(ended) if ended.k >= ATTEMPTS => {
                    return Ok(Ended::failed(
                        Exit::Failed.status(),
                        format!("{} attempts and none told how it went", ended.k),
                    ));
                }
                Some(ended) => self.attempt(step, ended.k + 1),
                None => self.attempt(step, 1),
            };
            match self
                .store
                .launch(self.fence, self.id, step.n, &attempt, machine)
            {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(Leave),
                Err(e) => {
                    return Ok(Ended::failed(
                        Exit::Failed.status(),
                        format!("the store: {e}"),
                    ));
                }
            }
            let records = self
                .home
                .submission(self.id)
                .join("steps")
                .join(format!("{}.{}.records", step.n, attempt.k));
            let _ = fs::remove_file(&records);
            let mut env: Vec<(&str, String)> = env.to_vec();
            env.push(("DIBS_ATTEMPT", attempt.id.clone()));
            env.push(("DIBS_TRAILERS", records.display().to_string()));
            if let Some(bundle) = &bundle {
                env.push(("DIBS_BUNDLE_SPEC", bundle.clone()));
            }
            if killed {
                env.push(("DIBS_ATTEMPT_LOOKUP", "1".into()));
            }
            // Once killed, a plain call only asks after the attempt, which the machine answers
            // before anything else, and is not itself cut short.
            let lookup = [
                "run".to_string(),
                "--on".into(),
                machine.into(),
                "--label".into(),
                "orchestrator-lookup".into(),
                "true".into(),
            ];
            let code = match killed {
                true => self.dibs_call(&log, &lookup, &env, None, false),
                false => self.dibs_call(&log, args, &env, cwd, true),
            };
            let heard = last_record(&records);
            let said = heard
                .as_ref()
                .and_then(|r| serde_json::to_string(r).ok())
                .unwrap_or_else(|| format!("{{\"exit\":{code}}}"));
            // The attempt is closed only with the step's outcome, or to make another: one left
            // open by a crash is asked about again under its own id, never run under a new one.
            let close = |ended: Ended| Ok(ended.of(&attempt.id, &said));
            let again = || {
                let _ = self.store.attempt_ended(&attempt.id, &said);
            };
            let killed = self.store.cancelled(self.id).unwrap_or(false);
            if STOP.load(Ordering::SeqCst) && !killed {
                return Err(Leave);
            }
            if killed {
                match &heard {
                    Some(Record::Admitted(Admitted::Running { job, pid })) => {
                        // Its call belongs to a worker before this one, so the job is stopped on
                        // its machine; the attempt stays open until it is seen to end.
                        let _ = append(&log, &format!("stopping job {job} of an earlier worker\n"));
                        self.dibs_call(
                            &log,
                            &[
                                "--on".into(),
                                machine.into(),
                                "--kill".into(),
                                pid.to_string(),
                            ],
                            &[],
                            None,
                            false,
                        );
                        std::thread::sleep(still_running());
                        continue;
                    }
                    // Until the machine answers, nobody knows whether the job still runs.
                    None => {
                        let _ = append(
                            &log,
                            &format!("{machine} did not answer (exit {code}); asking again\n"),
                        );
                        std::thread::sleep(match away(code) {
                            true => UNREACHABLE,
                            false => UNHEARD,
                        });
                        continue;
                    }
                    _ => {
                        return close(Ended::killed(
                            trailer_in(heard.as_ref()),
                            "killed with dibs orch kill",
                        ));
                    }
                }
            }
            match heard {
                Some(Record::Trailer(trailer))
                | Some(Record::Admitted(Admitted::Ended(trailer))) => {
                    let gone = trailer.by == By::Dibs && [129, 130, 143].contains(&trailer.exit);
                    match (trailer.exit, gone, kind) {
                        (0, _, _) => return close(Ended::done(Some(trailer), None)),
                        (_, true, Kind::Again) => again(),
                        (exit, true, Kind::Once) => {
                            return close(Ended {
                                state: State::Interrupted,
                                exit,
                                trailer: Some(trailer),
                                note: Some("the measurement was stopped when its caller went, and is not run again".into()),
                                attempt: None,
                            });
                        }
                        (exit, false, _) => {
                            return close(Ended {
                                state: State::Failed,
                                exit,
                                trailer: Some(trailer),
                                note: None,
                                attempt: None,
                            });
                        }
                    }
                }
                Some(Record::Admitted(Admitted::Running { job, .. })) => {
                    let _ = append(
                        &log,
                        &format!(
                            "job {job} still runs this attempt; asking again in {}s\n",
                            still_running().as_secs()
                        ),
                    );
                    // Told to stop, the next pass stops the job if it was a kill, or leaves.
                    pause(still_running());
                }
                Some(Record::Admitted(Admitted::Lost { started: false, .. })) => again(),
                Some(Record::Admitted(Admitted::Lost { job, .. })) => match kind {
                    Kind::Again => again(),
                    Kind::Once => {
                        return close(Ended {
                            state: State::Unknown,
                            exit: Exit::Failed.status(),
                            trailer: None,
                            note: Some(format!(
                                "job {job} ran, and its runner went without saying how: not run again"
                            )),
                            attempt: None,
                        });
                    }
                },
                // Away, the runner may have kept how the attempt ended: it is asked again
                // under the same id, which runs nothing twice, for as long as the machine is away.
                None if away(code) => {
                    let _ = append(
                        &log,
                        &format!(
                            "{machine} cannot take the call now (exit {code}); asking again in {}s\n",
                            UNREACHABLE.as_secs()
                        ),
                    );
                    pause(UNREACHABLE);
                }
                Some(_) | None => {
                    unheard += 1;
                    if unheard < ATTEMPTS {
                        pause(UNHEARD);
                        continue;
                    }
                    return close(Ended::failed(
                        code,
                        format!(
                            "dibs exited {code} without a trailer from {machine}, {unheard} times"
                        ),
                    ));
                }
            }
        }
    }

    fn attempt(&self, step: &Step, k: i64) -> Attempt {
        Attempt {
            id: format!("{}.{}.{k}", self.id, step.n),
            k,
            open: true,
        }
    }

    /// One dibs call, its output appended to the step's log; its exit, or what stopped it.
    fn dibs_logged(
        &self,
        log: &Path,
        args: &[String],
        env: &[(&str, String)],
        cwd: Option<&Path>,
    ) -> i32 {
        self.dibs_call(log, args, env, cwd, true)
    }

    /// A `stoppable` call is stopped when this worker is told to stop.
    fn dibs_call(
        &self,
        log: &Path,
        args: &[String],
        env: &[(&str, String)],
        cwd: Option<&Path>,
        stoppable: bool,
    ) -> i32 {
        let shown: Vec<String> = args.iter().map(|a| shell_word(a)).collect();
        let _ = append(log, &format!("$ dibs {}\n", shown.join(" ")));
        let Ok(out) = File::options().create(true).append(true).open(log) else {
            return Exit::Failed.status();
        };
        let mut command = dibs();
        command
            .args(args)
            // The split's profile alone says whether it builds in an image.
            .env_remove("DIBS_IMAGE")
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .env("DIBS_NO_PROVISION", "1")
            .env("DIBS_CHECK_IN", "off")
            .env(
                "DIBS_AGENT",
                format!(
                    "orchestrator {} for {}/{}",
                    self.id, self.coworker, self.submission.agent
                ),
            )
            .env("DIBS_USER", self.coworker)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_SUBAGENT_ID")
            .stdin(Stdio::null())
            .stdout(out.try_clone().map_or(Stdio::null(), Stdio::from))
            .stderr(out);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        // SAFETY: setpgid is async-signal-safe; the call gets a group a kill can stop whole.
        unsafe {
            command.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                let _ = append(log, &format!("dibs could not start: {e}\n"));
                return Exit::Failed.status();
            }
        };
        let mut stopped = false;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Exit::shell_status(status),
                Ok(None) => {}
                Err(_) => return Exit::Failed.status(),
            }
            if stoppable && STOP.load(Ordering::SeqCst) && !stopped {
                stopped = true;
                // SAFETY: kill only sends a signal, to the group this worker made.
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM) };
            }
            std::thread::sleep(TICK);
        }
    }
}

/// The exit a submission or step that ended in `state` reports: never 0 unless it is done.
fn outcome(state: State, exit: i32) -> i32 {
    match state {
        State::Done => 0,
        _ if exit == 0 => Exit::Failed.status(),
        _ => exit,
    }
}

/// Exits that say the machine could not take the call at all, for now: unreachable, full, its
/// lock directory unwritable, or no runner of this version yet.
fn away(code: i32) -> bool {
    [
        Exit::Unreachable,
        Exit::NoRoom,
        Exit::NoLock,
        Exit::NoRunner,
    ]
    .iter()
    .any(|e| e.status() == code)
}

fn trailer_in(record: Option<&Record>) -> Option<Trailer> {
    match record? {
        Record::Trailer(t) | Record::Admitted(Admitted::Ended(t)) => Some(t.clone()),
        _ => None,
    }
}

/// The last trailer or admission the call heard: what says how its job went.
fn last_record(path: &Path) -> Option<Record> {
    let file = File::open(path).ok()?;
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Record>(&line).ok())
        .filter(|r| matches!(r, Record::Trailer(_) | Record::Admitted(_)))
        .last()
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    File::options()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// Waits `for_`, unless told to stop first: true when the whole wait passed.
fn pause(for_: Duration) -> bool {
    let until = std::time::Instant::now() + for_;
    while std::time::Instant::now() < until {
        if STOP.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(TICK);
    }
    true
}

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn listen_for_stop() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic.
        unsafe { libc::signal(signal, stop as *const () as libc::sighandler_t) };
    }
}

/// An ELF with no program interpreter loads nothing, so ldd has nothing to check, and its exit
/// for one differs between versions.
fn is_static(exe: &Path) -> bool {
    const PT_INTERP: u32 = 3;
    let Ok(elf) = fs::read(exe) else {
        return false;
    };
    let at = |o: usize, n: usize| elf.get(o..o + n);
    let u = |o: usize, n: usize| {
        at(o, n).map(|b| b.iter().rev().fold(0u64, |v, &x| (v << 8) | u64::from(x)))
    };
    // 64-bit little-endian only, the one kind of executable a split builds.
    if at(0, 6) != Some(b"\x7fELF\x02\x01".as_slice()) {
        return false;
    }
    let (Some(off), Some(size), Some(count)) = (u(0x20, 8), u(0x36, 2), u(0x38, 2)) else {
        return false;
    };
    (0..count).all(|i| {
        let header = off as usize + (i * size) as usize;
        u(header, 4).is_some_and(|kind| kind != u64::from(PT_INTERP))
    })
}

/// Whether `pid` names a process, even one this account may not signal.
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    let answered = unsafe { libc::kill(pid as libc::pid_t, 0) };
    answered == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf(kinds: &[u32]) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[..6].copy_from_slice(b"\x7fELF\x02\x01");
        b[0x20..0x28].copy_from_slice(&64u64.to_le_bytes());
        b[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        b[0x38..0x3a].copy_from_slice(&(kinds.len() as u16).to_le_bytes());
        for kind in kinds {
            let mut header = vec![0u8; 56];
            header[..4].copy_from_slice(&kind.to_le_bytes());
            b.extend(header);
        }
        b
    }

    #[test]
    fn only_an_executable_without_an_interpreter_is_static() {
        let dir = std::env::temp_dir().join(format!("dibs-elf.{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for (name, bytes, expected) in [
            ("static", elf(&[1, 1]), true),
            ("dynamic", elf(&[6, 3, 1]), false),
            ("script", b"#!/bin/sh\n".to_vec(), false),
        ] {
            fs::write(dir.join(name), bytes).unwrap();
            assert_eq!(is_static(&dir.join(name)), expected, "{name}");
        }
        assert!(!is_static(&dir.join("absent")));
        let _ = fs::remove_dir_all(&dir);
    }
}
