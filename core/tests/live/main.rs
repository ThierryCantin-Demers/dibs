//! dibs against a real machine, the only way to show a job dying with the caller that started
//! it. Every test takes that machine's lock, one of them the exclusive lock, and kills its own
//! jobs there, so nothing runs this by accident: `cargo test` leaves it out (`test = false` in
//! Cargo.toml), and it runs only against a machine named twice, once to choose it and once to
//! say it is yours to use, and only while that machine is idle:
//!
//!     DIBS_LIVE_MACHINE=<machine> DIBS_LIVE_CONFIRM=<machine> cargo test --test live
//!
//! The machine has to accept --bench. The tests take turns, whatever the harness's thread count.

#[allow(dead_code)]
#[path = "../suite/harness.rs"]
mod harness;

use harness::{Call, Sandbox, Text, DIBS};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

const REFUSAL: &str = "These tests take a real machine's lock and kill their own jobs on it. \
Name the machine twice, once to choose it and once to say it is yours to use right now:\n\n    \
DIBS_LIVE_MACHINE=<machine> DIBS_LIVE_CONFIRM=<machine> cargo test --test live\n";

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// The machine under test, held by one test at a time.
struct Live {
    name: String,
    /// This run's own directory there, under the machine's scratch.
    dir: String,
    _turn: MutexGuard<'static, ()>,
}

impl Live {
    fn claim() -> Live {
        let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let name = std::env::var("DIBS_LIVE_MACHINE").unwrap_or_default();
        let confirm = std::env::var("DIBS_LIVE_CONFIRM").unwrap_or_default();
        assert!(!name.is_empty() && name == confirm, "{REFUSAL}");
        let mut live = Live { name, dir: String::new(), _turn: turn };
        let status = live.dibs(["--status"]).run();
        assert_eq!(status.code, 0, "{} cannot be reached, so nothing ran:\n{}", live.name, status.all());
        // The test before this one may still be letting go; anyone else's job means no.
        let idle = live.eventually(Duration::from_secs(10), || live.status().contains("dibs: idle"));
        assert!(idle, "{} is in use, so nothing ran:\n{}", live.name, live.status());
        live.dir = live.peek(&format!("d=\"$DIBS_SCRATCH/tmp/dibs-live-{}\"; mkdir -p \"$d\" && echo \"$d\"", std::process::id()));
        assert!(live.dir.starts_with('/'), "no directory of its own on {}: {:?}", live.name, live.dir);
        live
    }

    fn dibs<I, S>(&self, args: I) -> Call
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cmd = Command::new(DIBS);
        cmd.args(args.into_iter().map(|a| a.as_ref().to_string()));
        cmd.env("DIBS_ON", &self.name).env_remove("DIBS_LOCAL").env_remove("DIBS_HOST");
        Call::wrap(cmd)
    }

    fn peek(&self, script: &str) -> String {
        self.dibs(["--peek", script]).run().stdout.trim().to_string()
    }

    fn status(&self) -> String {
        self.dibs(["--status"]).run().stdout
    }

    /// Asks the machine again every quarter second until the answer changes, within a bound: each
    /// question is an ssh round trip, so this is gentler on it than the local suite's polling.
    fn eventually(&self, limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if cond() {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn soon(&self, cond: impl FnMut() -> bool) -> bool {
        self.eventually(Duration::from_secs(60), cond)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // Only the directory this run made, under a name no one else uses.
        if self.dir.contains("/tmp/dibs-live-") {
            self.peek(&format!("rm -rf '{}'", self.dir));
        }
    }
}

fn kill9(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

#[test]
fn a_job_dies_with_the_caller_that_started_it() {
    let live = Live::claim();
    let mut s = Sandbox::new();
    let tag = format!("dibs-live-{}", std::process::id());
    // Blocks without using a CPU, and carries a tag that names only this run's process.
    let workload = format!("python3 -c 'import signal; signal.pause()' {tag}");
    let caller = s.spawn(live.dibs(["--label", "live-hangup", "--max", "120", &workload]));
    assert!(live.soon(|| live.status().contains("live-hangup")), "it is holding the lock");
    let find = format!("pgrep -f '^python3 -c import signal.* {tag}$' | head -1");
    let mut work = String::new();
    assert!(live.soon(|| { work = live.peek(&find); !work.is_empty() }), "its workload is running over there");
    kill9(caller.pid);
    s.wait(caller);
    let alive = format!("ps -p {work} > /dev/null && echo alive || echo gone");
    assert!(live.soon(|| live.peek(&alive) == "gone"), "the workload died with it");
    assert!(live.soon(|| live.status().contains("dibs: idle")), "the lock came back");
}

#[test]
fn a_caller_that_dies_while_its_job_is_queued_takes_it_out_of_the_queue() {
    let live = Live::claim();
    let mut s = Sandbox::new();
    let (gate, marker) = (format!("{}/gate", live.dir), format!("{}/marker", live.dir));
    live.peek(&format!("mkfifo '{gate}'"));
    let holder = s.spawn(live.dibs(["--bench", "--max", "60", "--label", "live-holder", &format!("read -r _ < '{gate}'")]));
    assert!(live.soon(|| live.status().contains("live-holder")), "a benchmark holds the machine");
    let queued = s.spawn(live.dibs(["--label", "live-queued", &format!("echo ran > '{marker}'")]));
    assert!(live.soon(|| live.status().contains("live-queued")), "it is queued behind the benchmark");
    kill9(queued.pid);
    s.wait(queued);
    assert!(live.soon(|| !live.status().contains("live-queued")), "killing its caller drops it from the queue");
    assert_eq!(live.peek(&format!("test -e '{marker}' && echo ran || echo never")), "never", "and it never ran");
    assert_eq!(live.dibs(["--log", "20"]).run().stdout.lines_matching("caller-gone.*live-queued"), 1, "the log says why");
    live.peek(&format!("printf 'go\\n' > '{gate}'"));
    assert_eq!(s.wait(holder), 0);
}

#[test]
fn a_watch_dies_with_the_terminal_that_was_watching() {
    let live = Live::claim();
    let mut s = Sandbox::new();
    let out = s.path("watch.out");
    let watch = s.spawn(live.dibs(["--watch", "2"]).stdout_to(&out));
    // The script's name there carries the pid of the dibs that sent it, which picks out this
    // watch from any dibstop or person watching beside it.
    let running = format!("pgrep -f '[.]dibs-payload[.]{}[.]' > /dev/null && echo running || echo gone", watch.pid);
    assert!(live.soon(|| std::fs::read_to_string(&out).unwrap_or_default().contains("ctrl-c to stop")), "it draws");
    assert_eq!(live.peek(&running), "running", "the loop is running over there");
    kill9(watch.pid);
    s.wait(watch);
    assert!(live.soon(|| live.peek(&running) == "gone"), "and it stops when the caller does");
}

#[test]
fn rsync_reaches_the_machine_through_the_lock() {
    let live = Live::claim();
    let s = Sandbox::new();
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let blob: Vec<u8> = (0..20_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    s.write("sync/a.txt", "one\n");
    std::fs::write(s.path("sync/b.bin"), &blob).unwrap();
    let (here, there) = (format!("{}/", s.p("sync")), format!(":{}/sync/", live.dir));
    live.dibs(["--sync", "-a", &here, &there]).run();
    assert_eq!(live.peek(&format!("cat '{}/sync/a.txt'", live.dir)), "one", "a tree lands over there");
    live.peek(&format!("touch '{}/sync/stale'", live.dir));
    live.dibs(["--sync", "-a", "--delete", &here, &there]).run();
    assert_eq!(live.peek(&format!("ls '{}/sync'", live.dir)).split_whitespace().collect::<Vec<_>>(), ["a.txt", "b.bin"], "--delete takes away what is no longer here");
    live.dibs(["--sync", "-a", &there, &format!("{}/", s.p("back"))]).run();
    assert_eq!(
        (s.read("back/a.txt"), std::fs::read(s.path("back/b.bin")).ok()),
        ("one\n".to_string(), Some(blob)),
        "and the whole tree comes back byte for byte"
    );
    assert!(live.dibs(["--log", "30"]).run().stdout.lines_with(" sync ") >= 1, "it was a job like any other");
}

#[test]
fn the_ordinary_paths_work_over_ssh() {
    let live = Live::claim();
    assert_eq!(live.dibs(["--label", "live-run", "echo hello-over-ssh"]).run().stdout, "hello-over-ssh\n", "a run returns its output");
    assert_eq!(live.peek("echo peeked"), "peeked", "--peek needs no lock");
    // With a terminal, every tool downstream thinks it is interactive and git opens its pager.
    assert_eq!(live.peek("[ -t 0 ] || [ -t 1 ] && echo tty || echo no-tty"), "no-tty", "no tty, so nothing pages");
    assert_eq!(live.dibs(["--log", "20"]).run().stdout.lines_with("live-run"), 2, "the log recorded the run");
}
