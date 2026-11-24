//! A submission survives its worker and its store: a duplicate is one submission, a worker
//! killed mid-step is carried on by the next without running the job twice, a restored store
//! asks how the attempts it left open went, and a measurement whose runner vanished is never
//! measured again.
//!
//! `dibs` is a script here that admits each attempt once, as a runner does: asked again, it says
//! the attempt still runs, how it ended, or that it was lost.

use dibs_format::orchestrator::{
    API_COMMAND, Ask, Call, NamedCall, Reply, State, Submission, VERSION, Work,
};
use std::{
    fs,
    io::{BufRead as _, BufReader, Write as _},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc,
    time::Duration,
};

const WITHIN: Duration = Duration::from_secs(60);

const FAKE_DIBS: &str = r#"#!/bin/sh
F="$FAKE"
if [ "$3" = --kill ]; then kill "$4"; exit 0; fi
echo "$DIBS_ATTEMPT $*" >> "$F/calls"
a="$F/attempts/$DIBS_ATTEMPT"
say() { printf '%s\n' "$1" >> "$DIBS_TRAILERS"; }
if [ "$DIBS_ATTEMPT_LOOKUP" = 1 ] && [ ! -d "$a" ]; then say '{"admitted":"never"}'; exit 1; fi
if ! mkdir "$a" 2>/dev/null; then
  if [ -f "$a/ended" ]; then say "{\"admitted\":{\"ended\":$(cat "$a/ended")}}"; exit "$(cat "$a/exit")"; fi
  if [ -f "$a/pid" ] && kill -0 "$(cat "$a/pid")" 2>/dev/null; then
    say "{\"admitted\":{\"running\":{\"job\":\"1-$(cat "$a/pid")\",\"pid\":$(cat "$a/pid")}}}"; exit 75
  fi
  say '{"admitted":{"lost":{"job":"1-0","started":true}}}'; exit 1
fi
echo $$ > "$a/pid"
echo $PPID > "$F/worker"
echo "$DIBS_ATTEMPT" >> "$F/ran"
if [ -p "$F/gate" ]; then echo up > "$F/reached"; read -r _ < "$F/gate"; fi
body="{\"job\":\"1-$$\",\"mode\":\"shared\",\"label\":\"l\",\"queued\":0,\"ran\":1,\"exit\":0,\"by\":\"command\",\"built\":null}"
echo 0 > "$a/exit"
echo "$body" > "$a/ended.part" && mv "$a/ended.part" "$a/ended"
rm -f "$a/pid"
say "{\"trailer\":$body}"
"#;

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "dibs-orch-{name}-{}-{}",
            std::process::id(),
            dibs_format::Moment::epoch_now()
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in ["home", "fake/attempts"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        let dibs = root.join("dibs");
        fs::write(&dibs, FAKE_DIBS).unwrap();
        fs::set_permissions(&dibs, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("machines.toml"),
            "[machine.box-a]\nssh = \"box-a\"\n",
        )
        .unwrap();
        Sandbox { root }
    }

    fn fake(&self) -> PathBuf {
        self.root.join("fake")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dibs-orchestrator"));
        command
            .args(args)
            .env("DIBS_ORCH_HOME", self.root.join("home"))
            .env("DIBS_ORCH_DIBS", self.root.join("dibs"))
            .env("DIBS_MACHINES", self.root.join("machines.toml"))
            .env("DIBS_ORCH_SPAWN", "setsid")
            .env("DIBS_ORCH_RECHECK", "1")
            .env("FAKE", self.fake());
        command
    }

    fn orchestrator(&self, args: &[&str]) -> Output {
        let out = self.command(args).output().unwrap();
        assert!(
            out.status.success(),
            "dibs-orchestrator {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// One API call as alice: every reply, then the exit `End` gave.
    fn ask(&self, call: Call) -> (Vec<Reply>, i32) {
        let mut child = self
            .command(&["serve", "alice"])
            .env("SSH_ORIGINAL_COMMAND", API_COMMAND)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = serde_json::to_vec(&Ask { v: VERSION, call }).unwrap();
        line.push(b'\n');
        child.stdin.take().unwrap().write_all(&line).unwrap();
        let mut replies = Vec::new();
        let mut exit = None;
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            match serde_json::from_str::<Reply>(&line.unwrap()).unwrap() {
                Reply::End { exit: code } => exit = Some(code),
                reply => replies.push(reply),
            }
        }
        child.wait().unwrap();
        (replies, exit.expect("the call ends with End"))
    }

    fn submit(&self, key: &str, bench: bool) -> (String, bool) {
        let work = Work::Call(NamedCall {
            on: "box-a".into(),
            bench,
            device: bench.then(|| "gpu:card".into()),
            label: "l".into(),
            command: "true".into(),
            max: None,
        });
        let (replies, exit) = self.ask(Call::Submit(Box::new(Submission {
            key: key.into(),
            agent: "test".into(),
            work,
            upload: None,
        })));
        assert_eq!(exit, 0, "{replies:?}");
        replies
            .into_iter()
            .find_map(|r| match r {
                Reply::Accepted { id, duplicate } => Some((id, duplicate)),
                _ => None,
            })
            .expect("accepted")
    }

    /// Waits for the submission's worker to let go, then how it ended.
    fn follow(&self, id: &str) -> (State, i32) {
        let (replies, exit) = self.ask(Call::Follow {
            id: id.into(),
            check_in: WITHIN.as_secs(),
        });
        let state = replies
            .into_iter()
            .find_map(|r| match r {
                Reply::Status(detail) => Some(detail.summary.state),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{id} did not end within {WITHIN:?}: {}", self.log(id)));
        (state, exit)
    }

    fn ran(&self) -> usize {
        fs::read_to_string(self.fake().join("ran"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    fn calls(&self) -> usize {
        fs::read_to_string(self.fake().join("calls"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    fn log(&self, id: &str) -> String {
        fs::read_to_string(
            self.root
                .join("home/state/subs")
                .join(id)
                .join("worker.log"),
        )
        .unwrap_or_default()
    }

    fn hold_jobs(&self) {
        for fifo in ["gate", "reached"] {
            mkfifo(&self.fake().join(fifo));
        }
    }

    /// Blocks until a job is held at the gate.
    fn reached(&self) {
        let reached = self.fake().join("reached");
        let (done, waited) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = fs::read(&reached);
            let _ = done.send(());
        });
        waited
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("no job reached the gate: {:?}", fs::read_dir(&self.root)));
    }

    /// Lets the held job end.
    fn open_gate(&self) {
        let mut gate = fs::File::options()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(self.fake().join("gate"))
            .expect("a job waits at the gate");
        gate.write_all(b"go\n").unwrap();
        drop(gate);
        let _ = fs::remove_file(self.fake().join("gate"));
    }

    /// Kills the worker that started the held job, as a crash would.
    fn kill_worker(&self) {
        let pid: i32 = fs::read_to_string(self.fake().join("worker"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: kill only sends a signal.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let lock = fs::read_dir(self.root.join("home/state/subs"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("worker.lock");
        // The kernel lets go of the dead worker's lock when its last descriptor closes.
        fs::File::open(lock).unwrap().lock().unwrap();
    }

    /// Kills the held job too, as a runner that vanished mid-job leaves it.
    fn kill_job(&self) {
        let attempts = fs::read_dir(self.fake().join("attempts")).unwrap();
        for attempt in attempts {
            let pid = attempt.unwrap().path().join("pid");
            if let Ok(pid) = fs::read_to_string(&pid) {
                // SAFETY: kill only sends a signal.
                unsafe { libc::kill(pid.trim().parse().unwrap(), libc::SIGKILL) };
            }
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn mkfifo(path: &Path) {
    let c = std::ffi::CString::new(path.as_os_str().to_str().unwrap()).unwrap();
    // SAFETY: c is a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
}

#[test]
fn a_submission_sent_twice_under_one_key_is_one_submission() {
    let sandbox = Sandbox::new("duplicate");
    let (id, duplicate) = sandbox.submit("k1", false);
    assert!(!duplicate);
    let (again, duplicate) = sandbox.submit("k1", false);
    assert_eq!((again.as_str(), duplicate), (id.as_str(), true));
    assert_eq!(
        sandbox.follow(&id),
        (State::Done, 0),
        "{}",
        sandbox.log(&id)
    );
    assert_eq!(sandbox.ran(), 1);
    let (other, duplicate) = sandbox.submit("k2", false);
    assert!(!duplicate && other != id);
    assert_eq!(sandbox.follow(&other), (State::Done, 0));
}

#[test]
fn a_worker_killed_mid_step_is_carried_on_without_running_the_job_twice() {
    let sandbox = Sandbox::new("restart");
    sandbox.hold_jobs();
    let (id, _) = sandbox.submit("k", false);
    sandbox.reached();
    sandbox.kill_worker();
    sandbox.orchestrator(&["resume"]);
    sandbox.open_gate();
    assert_eq!(
        sandbox.follow(&id),
        (State::Done, 0),
        "{}",
        sandbox.log(&id)
    );
    assert_eq!(sandbox.ran(), 1, "{}", sandbox.log(&id));
    assert!(
        sandbox.calls() >= 2,
        "the new worker asked about the attempt"
    );
}

#[test]
fn a_restored_store_asks_how_its_open_attempts_went_rather_than_running_them_again() {
    let sandbox = Sandbox::new("restore");
    sandbox.hold_jobs();
    let (id, _) = sandbox.submit("k", false);
    sandbox.reached();
    let backup = sandbox.root.join("backup.db");
    sandbox.orchestrator(&["backup", backup.to_str().unwrap()]);
    sandbox.open_gate();
    assert_eq!(
        sandbox.follow(&id),
        (State::Done, 0),
        "{}",
        sandbox.log(&id)
    );
    let missing = sandbox.root.join("no-such-backup.db");
    let refused = sandbox
        .command(&["restore", missing.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!refused.status.success(), "a missing backup is no store");
    assert!(!missing.exists(), "nor is it made one");
    let out = sandbox.orchestrator(&["restore", backup.to_str().unwrap()]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&format!("{id}: resumed")),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        sandbox.follow(&id),
        (State::Done, 0),
        "{}",
        sandbox.log(&id)
    );
    assert_eq!(sandbox.ran(), 1, "{}", sandbox.log(&id));
}

#[test]
fn a_measurement_whose_runner_vanished_is_not_measured_again() {
    let sandbox = Sandbox::new("lost");
    sandbox.hold_jobs();
    let (id, _) = sandbox.submit("k", true);
    sandbox.reached();
    sandbox.kill_worker();
    sandbox.kill_job();
    sandbox.orchestrator(&["resume"]);
    let (state, exit) = sandbox.follow(&id);
    assert_eq!(state, State::Unknown, "{}", sandbox.log(&id));
    assert_ne!(exit, 0);
    assert_eq!(sandbox.ran(), 1);
}

#[test]
fn a_kill_stops_the_job_its_worker_runs() {
    let sandbox = Sandbox::new("kill");
    sandbox.hold_jobs();
    let (id, _) = sandbox.submit("k", false);
    sandbox.reached();
    let (_, exit) = sandbox.ask(Call::Kill { id: id.clone() });
    assert_eq!(exit, 0);
    assert_eq!(
        sandbox.follow(&id).0,
        State::Cancelled,
        "{}",
        sandbox.log(&id)
    );
    assert_eq!(sandbox.ran(), 1);
}

#[test]
fn a_kill_after_its_worker_died_stops_the_job_that_worker_left_running() {
    let sandbox = Sandbox::new("kill-recovered");
    sandbox.hold_jobs();
    let (id, _) = sandbox.submit("k", true);
    sandbox.reached();
    sandbox.kill_worker();
    let (_, exit) = sandbox.ask(Call::Kill { id: id.clone() });
    assert_eq!(exit, 0);
    assert_eq!(
        sandbox.follow(&id).0,
        State::Cancelled,
        "{}",
        sandbox.log(&id)
    );
    assert_eq!(sandbox.ran(), 1, "the measurement is never started again");
    let held = fs::read_dir(sandbox.fake().join("attempts"))
        .unwrap()
        .filter_map(|a| fs::read_to_string(a.ok()?.path().join("pid")).ok())
        .filter_map(|pid| pid.trim().parse::<i32>().ok())
        // SAFETY: signal 0 only asks whether the process lives.
        .filter(|pid| unsafe { libc::kill(*pid, 0) } == 0)
        .count();
    assert_eq!(held, 0, "the job the dead worker left is stopped");
}
