//! One API call: an `Ask` line in, `Reply` lines out, the last an `End`.

use crate::{
    home::{Home, component},
    inventory::Inventory,
    settings::Settings,
    spawn,
    split::{self, Refusal},
    store::{Planned, Store},
    worker,
};
use dibs_format::{
    Exit,
    orchestrator::{
        Ask, Call, NamedCall, Reply, STAGE_PREFIX, Seal, State, Submission, VERSION, Work,
    },
};
use std::{
    fs::File,
    io::{BufRead as _, Write as _},
    sync::mpsc,
    time::{Duration, Instant},
};

pub fn serve(home: &Home, coworker: &str) -> i32 {
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() || line.trim().is_empty() {
        return refuse("no request was read".into(), Exit::Refused.status());
    }
    let ask: Ask = match serde_json::from_str(&line) {
        Ok(ask) => ask,
        Err(e) => {
            return refuse(
                format!(
                    "the request cannot be read ({e}); this orchestrator speaks version {VERSION}"
                ),
                Exit::Refused.status(),
            );
        }
    };
    if ask.v != VERSION {
        return refuse(
            format!(
                "this orchestrator speaks version {VERSION} of the API, not {}: update dibs",
                ask.v
            ),
            Exit::Refused.status(),
        );
    }
    let store = match Store::open(&home.store()) {
        Ok(store) => store,
        Err(e) => {
            return refuse(
                format!("the store cannot be opened: {e}"),
                Exit::Failed.status(),
            );
        }
    };
    let api = Api {
        home,
        coworker,
        store: &store,
    };
    let exit = match ask.call {
        Call::Submit(submission) => api.submit(*submission),
        Call::List { all } => api.list(all),
        Call::Status { id } => api.status(&id),
        Call::Out { id, step, job } => api.out(&id, step.as_deref(), job),
        Call::Follow { id, check_in } => api.follow(&id, check_in),
        Call::Kill { id } => api.kill(&id),
        Call::Inventory => api.inventory(),
    };
    let exit = exit.unwrap_or_else(|(why, exit)| {
        send(&Reply::Text {
            err: true,
            text: format!("dibs: {why}\n"),
        });
        exit
    });
    send(&Reply::End { exit });
    exit
}

pub fn send(reply: &Reply) {
    if let Ok(mut line) = serde_json::to_vec(reply) {
        line.push(b'\n');
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&line);
        let _ = out.flush();
    }
}

/// Whether the submission's worker let go of it before `deadline`, if there is one.
fn worker_let_go(home: &Home, id: &str, deadline: Option<Instant>) -> bool {
    let lock = worker::lock_path(home, id);
    let (done, waited) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok(file) = File::open(&lock) {
            let _ = file.lock_shared();
        }
        let _ = done.send(());
    });
    match deadline {
        Some(deadline) => waited
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok(),
        None => waited.recv().is_ok(),
    }
}

fn say(text: impl Into<String>) {
    send(&Reply::Text {
        err: true,
        text: text.into(),
    });
}

fn refuse(why: String, exit: i32) -> i32 {
    say(format!("dibs: {why}\n"));
    send(&Reply::End { exit });
    exit
}

type Answer = Result<i32, (String, i32)>;

/// What a submission is shown as, its steps, and the split it was resolved from.
type Plan = (String, Vec<Planned>, Option<String>);

fn failed(e: impl std::fmt::Display) -> (String, i32) {
    (
        format!("the orchestrator's store failed: {e}"),
        Exit::Failed.status(),
    )
}

fn refused(why: impl Into<String>) -> (String, i32) {
    (why.into(), Exit::Refused.status())
}

struct Api<'a> {
    home: &'a Home,
    coworker: &'a str,
    store: &'a Store,
}

impl Api<'_> {
    fn submit(&self, submission: Submission) -> Answer {
        if !component(&submission.key) {
            return Err(refused(format!(
                "{:?} is no submission key",
                submission.key
            )));
        }
        if let Some(id) = self
            .store
            .find(self.coworker, &submission.key)
            .map_err(failed)?
        {
            self.started(&id)?;
            send(&Reply::Accepted {
                id,
                duplicate: true,
            });
            return Ok(0);
        }
        let inventory = Inventory::load();
        let (what, plan, resolved) = match &submission.work {
            Work::Call(call) => self.plan_call(call, &inventory)?,
            Work::Split(call) => {
                if !component(&call.repo)
                    || submission
                        .upload
                        .as_ref()
                        .is_some_and(|u| u.repo != call.repo)
                {
                    return Err(refused(format!(
                        "{:?} is no repo name, or not the repo the upload sent",
                        call.repo
                    )));
                }
                let settings = Settings::load(&self.home.settings()).map_err(refused)?;
                let pools = self.store.pools().map_err(failed)?;
                let text = std::fs::read_to_string(self.home.splits(&call.repo)).ok();
                let resolved = split::resolve(call, text.as_deref(), &settings, &pools, &inventory)
                    .map_err(|Refusal(why)| refused(why))?;
                self.seal(&submission)?;
                (
                    resolved.what(),
                    resolved.plan(),
                    serde_json::to_string(&resolved).ok(),
                )
            }
        };
        let (id, duplicate) = self
            .store
            .submit(
                self.coworker,
                &submission,
                &what,
                &plan,
                resolved.as_deref(),
            )
            .map_err(failed)?;
        self.started(&id)?;
        send(&Reply::Accepted { id, duplicate });
        Ok(0)
    }

    fn plan_call(&self, call: &NamedCall, inventory: &Inventory) -> Result<Plan, (String, i32)> {
        if !inventory.has(&call.on) {
            return Err(refused(format!(
                "the orchestrator knows no machine {}",
                call.on
            )));
        }
        if call.bench && call.device.is_none() {
            return Err(refused(
                "a measurement through the orchestrator names its card with --device",
            ));
        }
        let what = format!(
            "{} on {}{}: {}",
            match call.bench {
                true => "bench",
                false => "run",
            },
            call.on,
            call.device
                .as_deref()
                .map(|d| format!(" {d}"))
                .unwrap_or_default(),
            call.command
        );
        let plan = vec![Planned {
            name: "call".into(),
            machine: Some(call.on.clone()),
        }];
        Ok((what, plan, None))
    }

    /// The upload a split names, as it arrived, must be what the client sealed.
    fn seal(&self, submission: &Submission) -> Result<(), (String, i32)> {
        let Some(upload) = &submission.upload else {
            return Err(refused(
                "a split is built from your working tree, sent ahead as @local",
            ));
        };
        if upload.stage != submission.key || !component(&upload.repo) {
            return Err(refused("the upload names another stage or no repo"));
        }
        let stage = self
            .home
            .staging(self.coworker)
            .join(STAGE_PREFIX)
            .join(&upload.stage);
        let _held = self
            .home
            .stage_lock(self.coworker, &upload.stage)
            .map_err(|e| refused(format!("the stage's lock: {e}")))?;
        let listed = Seal::listed(&stage)
            .map_err(|e| refused(format!("nothing was staged under {}: {e}", upload.stage)))?;
        let (digest, files) = Seal::of(&stage, listed.iter().map(String::as_str))
            .map_err(|e| refused(format!("the staged tree cannot be read: {e}")))?;
        if digest != upload.digest || files != upload.files {
            return Err(refused(format!(
                "the staged tree is not what was sent ({files} files sealed {digest:.12}, sent {} sealed {:.12}): send it again",
                upload.files, upload.digest
            )));
        }
        let git = stage.join(".git");
        Upload::own_git(&git).map_err(|e| {
            refused(format!(
                "the staged repo's git directory cannot be taken over: {e}"
            ))
        })?;
        let head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&stage)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        match head {
            Some(head) if head == upload.head => Ok(()),
            other => Err(refused(format!(
                "the staged repo's HEAD is {}, not {}: send it again",
                other.as_deref().unwrap_or("unreadable"),
                upload.head
            ))),
        }
    }

    fn ended(&self, id: &str) -> bool {
        self.store
            .summary(id)
            .is_ok_and(|s| s.is_some_and(|s| s.state.ended()))
    }

    /// Its worker, started now unless one already holds it: whether one was.
    fn started(&self, id: &str) -> Result<bool, (String, i32)> {
        let summary = self.store.summary(id).map_err(failed)?;
        if summary.is_some_and(|s| s.state.ended()) || worker::driven(self.home, id) {
            return Ok(false);
        }
        spawn::worker(self.home, id, || self.ended(id)).map_err(|why| {
            (
                format!("submission {id} is stored, but {why}"),
                Exit::Failed.status(),
            )
        })
    }

    fn list(&self, all: bool) -> Answer {
        let submissions = self
            .store
            .list((!all).then_some(self.coworker))
            .map_err(failed)?;
        send(&Reply::Submissions(submissions));
        Ok(0)
    }

    fn status(&self, id: &str) -> Answer {
        let detail = self
            .store
            .detail(id)
            .map_err(failed)?
            .ok_or_else(|| refused(format!("no submission {id}")))?;
        let exit = detail.summary.exit.unwrap_or(0);
        send(&Reply::Status(detail));
        Ok(exit)
    }

    fn out(&self, id: &str, step: Option<&str>, job: bool) -> Answer {
        let detail = self
            .store
            .detail(id)
            .map_err(failed)?
            .ok_or_else(|| refused(format!("no submission {id}")))?;
        let steps: Vec<_> = detail
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| step.is_none_or(|name| s.name == name))
            .collect();
        if steps.is_empty() {
            return Err(refused(format!(
                "submission {id} has no step {}",
                step.unwrap_or("")
            )));
        }
        for (n, step) in steps {
            if job {
                let (Some(machine), Some(trailer)) = (&step.machine, &step.trailer) else {
                    continue;
                };
                let out = worker::dibs()
                    .args(["--on", machine, "--out", trailer.job.as_str()])
                    .output()
                    .map_err(|e| (format!("dibs could not be run: {e}"), Exit::Failed.status()))?;
                send(&Reply::Text {
                    err: false,
                    text: String::from_utf8_lossy(&out.stdout).into_owned(),
                });
                if !out.stderr.is_empty() {
                    say(String::from_utf8_lossy(&out.stderr).into_owned());
                }
                continue;
            }
            let log = worker::step_log(self.home, id, n as i64, &step.name);
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            send(&Reply::Text {
                err: false,
                text: format!("== {} ({})\n{text}", step.name, step.state.as_str()),
            });
        }
        Ok(0)
    }

    /// Waits for the worker to let go of the submission, or for `check_in` seconds, then says
    /// how each step went. One no worker drives is given one, once per call, so a crashed or
    /// stopped worker is never mistaken for an ending.
    fn follow(&self, id: &str, check_in: u64) -> Answer {
        if self.store.summary(id).map_err(failed)?.is_none() {
            return Err(refused(format!("no submission {id}")));
        }
        let within = Duration::from_secs(check_in.max(1));
        let deadline = Instant::now().checked_add(within);
        let mut revived = false;
        let detail = loop {
            if !revived && self.started(id)? {
                revived = true;
                say(format!(
                    "dibs: no worker drove submission {id}, so a new one carries it on from where it stopped\n"
                ));
            }
            if !worker_let_go(self.home, id, deadline) {
                say(format!(
                    "dibs: submission {id} still runs after {}s, and goes on without this call.\n  dibs orch follow {id}  follows it again\n  dibs orch kill {id}  stops it\n",
                    within.as_secs()
                ));
                return Ok(Exit::CheckedIn.status());
            }
            let detail = self
                .store
                .detail(id)
                .map_err(failed)?
                .ok_or_else(|| refused(format!("no submission {id}")))?;
            if detail.summary.state.ended() || revived {
                break detail;
            }
        };
        if !detail.summary.state.ended() {
            say(format!(
                "dibs: submission {id} has not ended, but its worker let go again; whoever runs the orchestrator resumes it\n"
            ));
        }
        for step in &detail.steps {
            if let Some(trailer) = &step.trailer {
                send(&Reply::Trailer {
                    step: step.name.clone(),
                    trailer: trailer.clone(),
                });
            }
        }
        let exit = detail.summary.exit.unwrap_or(match detail.summary.state {
            State::Done => 0,
            _ => Exit::Failed.status(),
        });
        send(&Reply::Status(detail));
        Ok(exit)
    }

    fn kill(&self, id: &str) -> Answer {
        let summary = self
            .store
            .summary(id)
            .map_err(failed)?
            .ok_or_else(|| refused(format!("no submission {id}")))?;
        if summary.coworker != self.coworker {
            return Err(refused(format!(
                "submission {id} is {}'s, so only they stop it",
                summary.coworker
            )));
        }
        if summary.state.ended() {
            say(format!(
                "dibs: submission {id} has already ended, {}\n",
                summary.state.as_str()
            ));
            return Ok(0);
        }
        let worker = self.store.cancel(id).map_err(failed)?;
        match worker.filter(|_| worker::driven(self.home, id)) {
            Some(pid) => {
                // SAFETY: kill only sends a signal.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                say(format!(
                    "dibs: submission {id} is being stopped: its worker stops its job\n"
                ));
            }
            None => {
                // A worker started now asks after any attempt left open, and stops its job,
                // before it records the kill.
                spawn::worker(self.home, id, || self.ended(id)).map_err(|why| {
                    (
                        format!("submission {id} is marked killed, but {why}"),
                        Exit::Failed.status(),
                    )
                })?;
                say(format!("dibs: submission {id} is being stopped\n"));
            }
        }
        Ok(0)
    }

    fn inventory(&self) -> Answer {
        send(&Reply::Inventory {
            machines: Inventory::load().text,
            pools: self.store.pools().map_err(failed)?,
        });
        Ok(0)
    }
}

/// A sent repo's git directory, made safe to run git in under the orchestrator's account.
pub fn own_git(git: &std::path::Path) -> std::io::Result<()> {
    Upload::own_git(git)
}

struct Upload;

impl Upload {
    /// What of a sent config is kept: how to read the repo and where it came from. Anything
    /// else in it, or in its hooks, could run a program of the sender's choosing here.
    const KEPT: &str = r"^(core\.repositoryformatversion|core\.bare|extensions\.(objectformat|refstorage)|remote\.origin\.url)$";

    fn own_git(git: &std::path::Path) -> std::io::Result<()> {
        let config = git.join("config");
        let kept = std::process::Command::new("git")
            .arg("config")
            .arg("--file")
            .arg(&config)
            .args(["--null", "--get-regexp", Upload::KEPT])
            .output()?;
        let owned = git.join("config.owned");
        let _ = std::fs::remove_file(&owned);
        std::fs::write(&owned, "")?;
        for entry in String::from_utf8_lossy(&kept.stdout)
            .split('\0')
            .filter(|e| !e.is_empty())
        {
            let Some((key, value)) = entry.split_once('\n') else {
                continue;
            };
            let set = std::process::Command::new("git")
                .arg("config")
                .arg("--file")
                .arg(&owned)
                .args([key, value])
                .status()?;
            if !set.success() {
                return Err(std::io::Error::other(format!("{key} could not be kept")));
            }
        }
        std::fs::rename(&owned, &config)?;
        for gone in [
            "hooks",
            "objects/info/alternates",
            "commondir",
            "config.worktree",
        ] {
            let path = git.join(gone);
            let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn a_sent_git_directory_keeps_how_to_read_it_and_nothing_that_runs() {
        let dir = std::env::temp_dir().join(format!("dibs-orch-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&dir)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        git(&["init", "-q"]);
        git(&["config", "core.fsmonitor", "touch ran"]);
        git(&["config", "extensions.worktreeConfig", "true"]);
        std::fs::write(
            dir.join(".git/config.worktree"),
            "[core]\n\tfsmonitor = touch ran\n",
        )
        .unwrap();
        git(&["config", "remote.origin.url", "https://example.com/app.git"]);
        std::fs::write(dir.join(".git/hooks/post-checkout"), "touch ran").unwrap();
        Upload::own_git(&dir.join(".git")).unwrap();
        let config = std::fs::read_to_string(dir.join(".git/config")).unwrap();
        assert!(!config.contains("fsmonitor"), "{config}");
        assert!(config.contains("https://example.com/app.git"), "{config}");
        assert!(config.contains("repositoryformatversion"), "{config}");
        assert!(!dir.join(".git/hooks").exists());
        git(&["status", "--short"]);
        assert!(!dir.join("ran").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
