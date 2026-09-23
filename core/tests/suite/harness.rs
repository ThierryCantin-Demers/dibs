//! A sandbox per test: its own lock directory, home, history and scratch, so every test runs
//! beside every other and none of them reads or writes the real ones. Nothing here reaches a real
//! machine: `DIBS_LOCAL=1` by default, and an ssh that fails at once for any name.

use regex::Regex;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DIBS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../bin/dibs");
pub const CORE: &str = env!("CARGO_BIN_EXE_dibs-core");

const CALL_LIMIT: Duration = Duration::from_secs(120);
const WAIT_LIMIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(5);

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

pub fn hostname() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let out = Command::new("hostname").arg("-s").output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    })
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// A pid that stays alive for the whole test, for records that prune would drop otherwise.
pub fn live_pid() -> u32 {
    std::process::id()
}

pub struct Sandbox {
    pub root: PathBuf,
    env: BTreeMap<String, String>,
    children: Vec<Spawned>,
}

/// A background job, reaped the moment it ends the way a shell reaps one: dibs asks whether a
/// pid is still there, and an unreaped zombie answers yes.
struct Spawned {
    pid: u32,
    exit: Arc<Mutex<Option<i32>>>,
    reaper: thread::JoinHandle<()>,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "dibs-suite.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in ["lockdir", "scratch", "runtime", "tmp", "bin", "nossh", "home/.cargo/bin"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::os::unix::fs::symlink(DIBS, root.join("bin/dibs")).unwrap();
        // Every machine named in a test is made up, and a real lookup of one takes seconds to
        // fail. This fails at once, the way ssh does for a name that does not resolve.
        let nossh = "#!/bin/bash\n\
            while [ $# -gt 0 ]; do case $1 in -[oiFJlpP]) shift 2 ;; -*) shift ;; *) break ;; esac; done\n\
            host=${1%%:*}; host=${host##*@}\n\
            echo \"$(basename \"$0\"): Could not resolve hostname $host: Name or service not known\" >&2\n\
            exit 255\n";
        write_exec(&root.join("nossh/ssh"), nossh);
        write_exec(&root.join("nossh/scp"), nossh);

        let at = |p: &str| root.join(p).display().to_string();
        let mut env = BTreeMap::new();
        let path = format!("{}:{}:{}", at("bin"), at("nossh"), std::env::var("PATH").unwrap_or_default());
        for (k, v) in [
            ("PATH", path),
            ("HOME", at("home")),
            ("XDG_CONFIG_HOME", at("home/.config")),
            ("XDG_STATE_HOME", at("home/.local/state")),
            ("XDG_CACHE_HOME", at("home/.cache")),
            ("XDG_RUNTIME_DIR", at("runtime")),
            ("TMPDIR", at("tmp")),
            ("DIBS_LOCAL", "1".into()),
            ("DIBS_LOCK_DIR", at("lockdir")),
            ("DIBS_HISTORY", at("history")),
            ("DIBS_LOG", at("log")),
            ("DIBS_SERIES", at("series")),
            ("DIBS_SEEN", at("seen")),
            ("DIBS_SCRATCH", at("scratch")),
            ("DIBS_CORE", CORE.into()),
            ("CLAUDE_CODE_HOST_SESSION_ID", "local_suite".into()),
        ] {
            env.insert(k.to_string(), v);
        }
        for k in ["USER", "LOGNAME", "LANG", "RUSTUP_HOME"] {
            if let Ok(v) = std::env::var(k) {
                env.insert(k.to_string(), v);
            }
        }
        Sandbox { root, env, children: Vec::new() }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// The same path as a string, for splicing into a command.
    pub fn p(&self, rel: &str) -> String {
        self.path(rel).display().to_string()
    }

    pub fn var(&self, key: &str) -> String {
        self.env.get(key).cloned().unwrap_or_default()
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        self.env.insert(key.to_string(), value.into());
    }

    pub fn unset(&mut self, key: &str) {
        self.env.remove(key);
    }

    /// Writes an inventory and points every later call at it.
    pub fn machines(&mut self, toml: &str) {
        let path = self.path("machines.toml");
        fs::write(&path, toml).unwrap();
        self.set("DIBS_MACHINES", path.display().to_string());
    }

    pub fn dibs<I, S>(&self, args: I) -> Call
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.command(DIBS, args)
    }

    /// Any program, with the sandbox's environment.
    pub fn command<I, S>(&self, program: &str, args: I) -> Call
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cmd = Command::new(program);
        cmd.env_clear().envs(&self.env).current_dir(&self.root);
        for a in args {
            cmd.arg(a.as_ref());
        }
        Call::wrap(cmd)
    }

    pub fn sh(&self, script: &str) -> Call {
        self.command("bash", ["-c", script])
    }

    pub fn status(&self) -> String {
        self.dibs(["--status"]).run().stdout
    }

    pub fn status_json(&self) -> serde_json::Value {
        let out = self.dibs(["--status", "--json"]).run();
        serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("--status --json: {e}\n{}", out.stdout))
    }

    /// Starts a call in the background, owned by the sandbox and killed with it.
    pub fn spawn(&mut self, call: Call) -> Job {
        let child = call.background();
        self.adopt(child)
    }

    /// Starts a call whose stdin is a pipe held here, as a machine's is the ssh channel: dropping
    /// it is how that side learns its caller has gone.
    pub fn spawn_fed(&mut self, mut call: Call) -> (Job, std::process::ChildStdin) {
        call.feed = true;
        let mut child = call.background();
        let feed = child.stdin.take().unwrap();
        (self.adopt(child), feed)
    }

    fn adopt(&mut self, mut child: Child) -> Job {
        let pid = child.id();
        let exit = Arc::new(Mutex::new(None));
        let set = exit.clone();
        let reaper = thread::spawn(move || {
            let code = child.wait().map(exit_code).unwrap_or(-1);
            *set.lock().unwrap() = Some(code);
        });
        self.children.push(Spawned { pid, exit, reaper });
        Job { pid }
    }

    /// Waits for a job started with `spawn` and returns its exit status.
    pub fn wait(&mut self, job: Job) -> i32 {
        let i = self.children.iter().position(|c| c.pid == job.pid).expect("not a job of this sandbox");
        let child = self.children.remove(i);
        let deadline = Instant::now() + CALL_LIMIT;
        loop {
            if let Some(code) = *child.exit.lock().unwrap() {
                let _ = child.reaper.join();
                return code;
            }
            if Instant::now() > deadline {
                unsafe { libc::kill(job.pid as i32, libc::SIGKILL) };
                panic!("job {} did not end within {CALL_LIMIT:?}", job.pid);
            }
            thread::sleep(POLL);
        }
    }

    pub fn gate(&self, name: &str) -> Gate {
        let path = self.path(&format!("f-{name}"));
        let _ = fs::remove_file(&path);
        let c = std::ffi::CString::new(path.display().to_string()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo {}", path.display());
        Gate { path }
    }

    pub fn count(&self, kind: &str) -> usize {
        let prefix = format!("{kind}.");
        fs::read_dir(self.lockdir())
            .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&prefix)).count())
            .unwrap_or(0)
    }

    pub fn lockdir(&self) -> PathBuf {
        PathBuf::from(self.var("DIBS_LOCK_DIR"))
    }

    pub fn holders(&self) -> usize {
        self.count("holder")
    }

    pub fn waiters(&self) -> usize {
        self.count("waiting")
    }

    pub fn held(&self, n: usize) {
        self.until_records(&format!("{n} holder(s)"), || self.holders() >= n);
    }

    pub fn queued(&self, n: usize) {
        self.until_records(&format!("{n} waiter(s)"), || self.waiters() >= n);
    }

    pub fn gone(&self) {
        self.until_records("every holder to go", || self.holders() == 0);
    }

    /// `until`, failing with what the lock directory held, which is the first thing to know.
    fn until_records(&self, what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT_LIMIT;
        while !cond() {
            if Instant::now() > deadline {
                let records: Vec<String> = fs::read_dir(self.lockdir())
                    .map(|d| d.flatten().map(|e| format!("{}: {}", e.file_name().to_string_lossy(), fs::read_to_string(e.path()).unwrap_or_default().trim_end())).collect())
                    .unwrap_or_default();
                panic!("timed out waiting for {what}; the lock directory holds:\n{}\nand the log ends:\n{}", records.join("\n"), self.log().lines().rev().take(6).collect::<Vec<_>>().join("\n"));
            }
            thread::sleep(POLL);
        }
    }

    /// Writes a lock record by hand, tab-separated, as a job would have.
    pub fn record(&self, kind: &str, pid: u32, fields: &[&str]) {
        fs::write(self.lockdir().join(format!("{kind}.{pid}")), format!("{}\n", fields.join("\t"))).unwrap();
    }

    /// The fields of every record of one kind, in no particular order.
    pub fn records(&self, kind: &str) -> Vec<Vec<String>> {
        let prefix = format!("{kind}.");
        let mut out = Vec::new();
        for e in fs::read_dir(self.lockdir()).unwrap().flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let text = fs::read_to_string(e.path()).unwrap_or_default();
                out.push(text.lines().next().unwrap_or("").split('\t').map(str::to_string).collect());
            }
        }
        out
    }

    /// The pid `--status` gives for the job under a label.
    pub fn pid_of(&self, label: &str) -> u32 {
        let status = self.status();
        let needle = format!(" {label} ");
        status
            .lines()
            .filter(|l| l.contains(&needle))
            .find_map(|l| {
                let words: Vec<_> = l.split_whitespace().collect();
                words.iter().position(|w| *w == "pid").and_then(|i| words.get(i + 1)?.parse().ok())
            })
            .unwrap_or_else(|| panic!("no pid for {label} in:\n{status}"))
    }

    pub fn history(&self, lines: &str) {
        append(&self.path("history"), lines);
    }

    pub fn set_history(&self, lines: &str) {
        fs::write(self.path("history"), lines).unwrap();
    }

    pub fn log(&self) -> String {
        fs::read_to_string(self.path("log")).unwrap_or_default()
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap_or_default()
    }

    pub fn write(&self, rel: &str, text: &str) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    pub fn write_exec(&self, rel: &str, text: &str) {
        self.write(rel, text);
        write_exec(&self.path(rel), text);
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }

    /// Waits for a line the log has not had yet, as a job's end is only visible there.
    pub fn log_line(&self, re: &str) {
        let re = Regex::new(re).unwrap();
        until(&format!("a log line matching {re}"), || self.log().lines().any(|l| re.is_match(l)));
    }

    /// The machine script, as it is sent: every part of it, in order.
    pub fn payload(&self) -> String {
        let mut parts: Vec<_> = fs::read_dir(repo_root().join("lib/machine")).unwrap().flatten().map(|e| e.path()).collect();
        parts.sort();
        let script: String = parts.iter().map(|p| fs::read_to_string(p).unwrap()).collect();
        let path = self.path("payload");
        fs::write(&path, script).unwrap();
        path.display().to_string()
    }

    /// A call that crosses a transport to a machine that is this one: an ssh whose far side is
    /// a process of its own, in a session of its own, as a remote one is. Signalling or stopping
    /// the client then leaves the far side to learn of it the way a machine does, through its
    /// stdin, and never by a signal a real remote could not receive.
    pub fn remote(&self, call: Call) -> Call {
        self.write_exec(
            "fakessh/ssh",
            "#!/bin/bash\n\
             while [ $# -gt 0 ]; do case $1 in -o) shift 2 ;; -*) shift ;; *) break ;; esac; done\n\
             shift\n\
             setsid bash -c \"$*\" <&0 &\n\
             far=$!\n\
             trap 'exit 255' TERM\n\
             wait $far\n",
        );
        fs::create_dir_all(self.path("remote-run")).unwrap();
        call.env("PATH", format!("{}:{}", self.p("fakessh"), self.var("PATH")))
            .env("DIBS_LOCAL", "0")
            .env("DIBS_HOST", "fake-remote")
            .env("DIBS_HOSTNAME", "laptop-here")
            .env("DIBS_REMOTE_DIR", self.p("remote-run"))
    }

    pub fn git(&self, dir: &str, args: &[&str]) -> String {
        let out = self
            .command("git", ["-c", "user.email=t@t", "-c", "user.name=t"].iter().chain(args))
            .dir(&self.path(dir))
            .run();
        assert_eq!(out.code, 0, "git {args:?} in {dir}: {}", out.stderr);
        out.stdout.trim().to_string()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for c in self.children.drain(..) {
            if c.exit.lock().unwrap().is_none() {
                unsafe { libc::kill(c.pid as i32, libc::SIGKILL) };
            }
            let _ = c.reaper.join();
        }
        // Holders block on a fifo under the root, and removing the directory does not release
        // them. Everything started here inherited this home, which no other sandbox has.
        let home = format!("HOME={}/home", self.root.display()).into_bytes();
        for e in fs::read_dir("/proc").unwrap().flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
            if let Ok(env) = fs::read(e.path().join("environ")) {
                if env.split(|b| *b == 0).any(|v| v == home.as_slice()) {
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
        let _ = Command::new("pkill").arg("-f").arg(format!("{}/", self.root.display())).status();
        if !thread::panicking() || std::env::var_os("DIBS_SUITE_KEEP").is_none() {
            let _ = Command::new("chmod").args(["-R", "u+w"]).arg(&self.root).status();
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

pub struct Job {
    pub pid: u32,
}

pub struct Call {
    cmd: Command,
    stdin: Option<String>,
    sink: Sink,
    limit: Duration,
    feed: bool,
}

enum Sink {
    Null,
    Stdout(PathBuf),
    Both(PathBuf, PathBuf),
}

impl Call {
    pub fn wrap(cmd: Command) -> Call {
        Call { cmd, stdin: None, sink: Sink::Null, limit: CALL_LIMIT, feed: false }
    }

    pub fn env(mut self, key: &str, value: impl AsRef<str>) -> Call {
        self.cmd.env(key, value.as_ref());
        self
    }

    pub fn env_remove(mut self, key: &str) -> Call {
        self.cmd.env_remove(key);
        self
    }

    /// A caller with no session: a person at a shell, or a runtime that publishes none.
    pub fn no_session(self) -> Call {
        self.env_remove("CLAUDE_CODE_HOST_SESSION_ID").env_remove("CLAUDE_CODE_SESSION_ID")
    }

    pub fn session(self, id: &str) -> Call {
        self.env("CLAUDE_CODE_HOST_SESSION_ID", format!("local_{id}"))
    }

    pub fn dir(mut self, dir: &Path) -> Call {
        self.cmd.current_dir(dir);
        self
    }

    pub fn stdin(mut self, text: &str) -> Call {
        self.stdin = Some(text.to_string());
        self
    }

    /// A tighter bound than the default, for a call whose failure mode is hanging.
    pub fn within(mut self, limit: Duration) -> Call {
        self.limit = limit;
        self
    }

    pub fn run(mut self) -> Output {
        self.cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        self.cmd.stdin(if self.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        // A group of its own only where the call may be cut short, so that cutting it takes
        // everything it started; process groups are otherwise part of what dibs reads.
        let bounded = self.limit < CALL_LIMIT;
        if bounded {
            use std::os::unix::process::CommandExt;
            self.cmd.process_group(0);
        }
        let what = format!("{:?}", self.cmd);
        let mut child = self.cmd.spawn().unwrap_or_else(|e| panic!("{what}: {e}"));
        if let Some(text) = self.stdin.take() {
            let mut pipe = child.stdin.take().unwrap();
            thread::spawn(move || {
                let _ = pipe.write_all(text.as_bytes());
            });
        }
        let out = drain(child.stdout.take().unwrap());
        let err = drain(child.stderr.take().unwrap());
        let deadline = Instant::now() + self.limit;
        let status = loop {
            if let Some(st) = child.try_wait().unwrap() {
                break st;
            }
            if Instant::now() > deadline {
                if bounded {
                    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                }
                let _ = child.kill();
                let _ = child.wait();
                return Output { code: 124, stdout: out.take(), stderr: err.take() };
            }
            thread::sleep(POLL);
        };
        Output { code: exit_code(status), stdout: out.take(), stderr: err.take() }
    }

    /// The exit status alone, with the output thrown away.
    pub fn code(self) -> i32 {
        self.run().code
    }

    /// Keeps a background job's stdout in a file, to be read after it ends.
    pub fn stdout_to(mut self, path: &Path) -> Call {
        self.sink = Sink::Stdout(path.to_path_buf());
        self
    }

    pub fn streams_to(mut self, stdout: &Path, stderr: &Path) -> Call {
        self.sink = Sink::Both(stdout.to_path_buf(), stderr.to_path_buf());
        self
    }

    fn background(mut self) -> Child {
        self.cmd.stdin(if self.feed { Stdio::piped() } else { Stdio::null() });
        match &self.sink {
            Sink::Null => self.cmd.stdout(Stdio::null()).stderr(Stdio::null()),
            Sink::Stdout(p) => self.cmd.stdout(fs::File::create(p).unwrap()).stderr(Stdio::null()),
            Sink::Both(o, e) => self.cmd.stdout(fs::File::create(o).unwrap()).stderr(fs::File::create(e).unwrap()),
        };
        self.cmd.spawn().unwrap()
    }
}

/// A stream read to its end on a thread of its own, as far as it gets.
struct Drain {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    done: thread::JoinHandle<()>,
}

fn drain(mut r: impl Read + Send + 'static) -> Drain {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let fill = buf.clone();
    let done = thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = r.read(&mut chunk) {
            if n == 0 {
                break;
            }
            fill.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    Drain { buf, done }
}

impl Drain {
    /// What was read, once the stream ends or a moment after: something the call left running
    /// may hold the pipe open for ever, and that is a leak to see, not a reason to hang.
    fn take(self) -> String {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.done.is_finished() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        let buf = self.buf.lock().unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }
}

fn exit_code(st: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0))
}

#[derive(Debug)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// Both streams, stdout first. Their interleaving is lost, which counting lines never needs.
    pub fn all(&self) -> String {
        let mut s = self.stdout.clone();
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s + &self.stderr
    }
}

/// A fifo a job blocks on, so a holder costs no CPU and ends exactly when it is told.
pub struct Gate {
    pub path: PathBuf,
}

impl Gate {
    /// The command that blocks until the gate opens.
    pub fn hold(&self) -> String {
        format!("read -r _ < {}", self.path.display())
    }

    /// The command that says a point was reached, for `reached` to wait on.
    pub fn signal(&self) -> String {
        format!("echo up > {}", self.path.display())
    }

    /// Releases whoever is blocked on it, waiting for one to arrive if nobody is yet.
    pub fn open(&self) {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            match OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&self.path) {
                Ok(mut f) => {
                    let _ = f.write_all(b"go\n");
                    return;
                }
                Err(_) if Instant::now() < deadline => thread::sleep(POLL),
                Err(e) => panic!("nothing ever read {}: {e}", self.path.display()),
            }
        }
    }

    /// Blocks until something writes to it.
    pub fn reached(&self) {
        let mut f = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&self.path).unwrap();
        let deadline = Instant::now() + WAIT_LIMIT;
        let mut buf = [0u8; 64];
        loop {
            if matches!(f.read(&mut buf), Ok(n) if n > 0) {
                return;
            }
            assert!(Instant::now() < deadline, "nothing reached {}", self.path.display());
            thread::sleep(POLL);
        }
    }
}

/// Polls a condition until it holds, failing the test after a bound rather than hanging it.
pub fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT_LIMIT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(POLL);
    }
}

pub fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

pub trait Text {
    /// How many lines contain `needle`, as `grep -c` counts.
    fn lines_with(&self, needle: &str) -> usize;
    /// How many lines match `re`.
    fn lines_matching(&self, re: &str) -> usize;
}

impl<T: AsRef<str>> Text for T {
    fn lines_with(&self, needle: &str) -> usize {
        self.as_ref().lines().filter(|l| l.contains(needle)).count()
    }

    fn lines_matching(&self, re: &str) -> usize {
        let re = Regex::new(re).unwrap();
        self.as_ref().lines().filter(|l| re.is_match(l)).count()
    }
}

/// The first group of `re` on the first line it matches.
pub fn capture(text: &str, re: &str) -> Option<String> {
    let re = Regex::new(re).unwrap();
    text.lines().find_map(|l| re.captures(l).map(|c| c[1].to_string()))
}

/// The lines from the first holding `from` through the next holding `to`, as `sed -n '/from/,/to/p'`.
pub fn between(text: &str, from: &str, to: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for l in text.lines() {
        inside |= l.contains(from);
        if inside {
            out.push_str(l);
            out.push('\n');
            if l.contains(to) {
                break;
            }
        }
    }
    out
}

/// The job id a trailer names.
pub fn job_id(stderr: &str) -> String {
    capture(stderr, r"^job ([0-9-]+) ").unwrap_or_else(|| panic!("no trailer in:\n{stderr}"))
}

pub fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn append(path: &Path, text: &str) {
    OpenOptions::new().create(true).append(true).open(path).unwrap().write_all(text.as_bytes()).unwrap();
}
