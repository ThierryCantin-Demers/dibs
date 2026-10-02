use crate::machine::{
    payload::{CallValues, Watch, encode},
    target::Target,
    unreachable::{Unreachable, no_room},
};
use dibs_format::Exit;
use std::{
    io::{self, Write as _},
    os::unix::{
        fs::OpenOptionsExt as _,
        process::{CommandExt as _, ExitStatusExt as _},
    },
    path::PathBuf,
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// A caller that says nothing for this long is gone, unless `DIBS_LEASE` says otherwise.
const DEFAULT_LEASE_SECS: u64 = 120;
const DEFAULT_CONNECT_TIMEOUT: &str = "10";
/// ssh's own failure, never the command's.
const SSH_FAILED: i32 = 255;
/// What the far line exits with when it could not write the script.
const SCRIPT_UNWRITTEN: i32 = 70;
/// The far side's `&&` continues a line the bash client split with a backslash.
const CONTINUATION: &str = "             ";

/// This computer, and whether every call stays on it.
#[derive(Debug, Clone)]
pub struct Here {
    /// Its name up to the first dot.
    pub name: String,
    /// `DIBS_LOCAL=1`.
    pub local: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The lock is taken on this computer.
    Here,
    Ssh {
        host: String,
    },
}

/// One call to one machine.
#[derive(Debug, Clone)]
pub struct Session {
    pub route: Route,
    /// The machine whose lock the call takes, lowercased.
    pub lock_at: String,
}

/// Where a command run here reaches the machine a session locks.
#[derive(Debug, Clone)]
pub enum Reach {
    Known(String),
    /// The address ssh dials for the host, asked only when a service needs it.
    Dialled {
        host: String,
        otherwise: String,
    },
}

impl Reach {
    pub fn address(&self) -> String {
        match self {
            Reach::Known(name) => name.clone(),
            Reach::Dialled { host, otherwise } => {
                Ssh::dials(host).unwrap_or_else(|| otherwise.clone())
            }
        }
    }
}

/// The caller's half of a job dying with it, from the environment.
#[derive(Debug, Clone, Copy)]
pub struct Liveness {
    /// `DIBS_NO_LIVE=1`: the caller's own stdin follows the script instead of a channel.
    pub no_live: bool,
    /// `DIBS_NO_WATCHDOG=1`: the machine does not watch the channel.
    pub no_watchdog: bool,
    /// `DIBS_NO_PDEATHSIG=1`: nothing started here is signalled when this process dies.
    pub no_pdeathsig: bool,
    pub lease: u64,
}

/// What the channel carries besides heartbeats.
#[derive(Debug, Clone, Copy)]
pub enum Message {
    /// A held command ended with this status, which is not the caller going away.
    Release(i32),
}

/// What follows the script down the machine half's stdin.
#[derive(Debug, Clone, Copy)]
enum Feed {
    /// Nothing: the script is all it reads.
    Close,
    /// Messages only, and the end when this process ends.
    Quiet,
    /// Messages, and a bare newline whenever this long passes without one.
    Heartbeat(Duration),
    /// This process's own stdin.
    Stdin,
}

/// The machine half of a call, started.
pub struct Started {
    pub child: Child,
    pub channel: Sender<Message>,
}

impl Liveness {
    pub fn from_env() -> Liveness {
        let on = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        let lease = std::env::var("DIBS_LEASE")
            .ok()
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_LEASE_SECS);
        Liveness {
            no_live: on("DIBS_NO_LIVE"),
            no_watchdog: on("DIBS_NO_WATCHDOG"),
            no_pdeathsig: on("DIBS_NO_PDEATHSIG"),
            lease,
        }
    }
}

impl Session {
    pub fn new(target: &Target, here: &Here) -> Session {
        let me = here.name.to_ascii_lowercase();
        match me == target.hostname.to_ascii_lowercase() || here.local {
            true => Session {
                route: Route::Here,
                lock_at: me,
            },
            false => Session {
                route: Route::Ssh {
                    host: target.host.clone(),
                },
                lock_at: target.hostname.to_ascii_lowercase(),
            },
        }
    }

    pub fn reach(&self, target: &Target, here: &Here) -> Reach {
        match &self.route {
            Route::Here => Reach::Known(here.name.clone()),
            Route::Ssh { host } => Reach::Dialled {
                host: host.clone(),
                otherwise: match target.hostname.is_empty() {
                    true => here.name.clone(),
                    false => target.hostname.clone(),
                },
            },
        }
    }

    /// Where notices say the lock is: the machine's name, or this computer's.
    pub fn at(&self, target: &Target, here: &Here) -> String {
        match (&self.route, &target.machine) {
            (Route::Here, _) => here.name.clone(),
            (Route::Ssh { .. }, Some(machine)) => machine.to_string(),
            (Route::Ssh { .. }, None) if !target.hostname.is_empty() => target.hostname.clone(),
            (Route::Ssh { .. }, None) => here.name.clone(),
        }
    }

    /// The exit a call gives for its machine half's: ssh's own failures are diagnosed here.
    pub fn exit(&self, status: i32, target: &Target) -> i32 {
        match (&self.route, status) {
            (Route::Ssh { .. }, SSH_FAILED) => {
                eprint!("{}", Unreachable { target }.diagnosis());
                i32::from(Exit::Unreachable.code())
            }
            (Route::Ssh { .. }, SCRIPT_UNWRITTEN) => {
                eprint!("{}", no_room(target));
                i32::from(Exit::NoRoom.code())
            }
            (_, status) => status,
        }
    }

    /// Runs a call whose command runs on the machine, and returns its exit.
    pub fn run(&self, values: &CallValues, half: &str, live: Liveness) -> io::Result<ExitStatus> {
        let deferred = Interrupt::defer();
        let started = self.start(values, half, live, false)?;
        let mut child = started.child;
        let status = child.wait();
        drop(deferred);
        drop(started.channel);
        let status = status?;
        Interrupt::pass_on(status);
        Ok(status)
    }

    /// Starts the machine half of a hold, with its stdout piped here.
    pub fn hold(&self, values: &CallValues, half: &str, live: Liveness) -> io::Result<Started> {
        self.start(values, half, live, true)
    }

    fn start(
        &self,
        values: &CallValues,
        half: &str,
        live: Liveness,
        hold: bool,
    ) -> io::Result<Started> {
        let (mut command, prelude, feed) = match &self.route {
            Route::Here => Session::here(values, half, hold)?,
            Route::Ssh { host } => Session::over_ssh(host, values, half, live, hold),
        };
        let die_with_me = !live.no_pdeathsig && !matches!((&self.route, hold), (Route::Here, true));
        // SAFETY: the closure makes async-signal-safe calls only.
        unsafe {
            command.pre_exec(move || {
                if hold {
                    libc::signal(libc::SIGINT, libc::SIG_IGN);
                    libc::signal(libc::SIGQUIT, libc::SIG_IGN);
                }
                if die_with_me {
                    parent_death_signal();
                }
                Ok(())
            });
        }
        command.stdin(Stdio::piped());
        if hold {
            command.stdout(Stdio::piped());
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let (channel, messages) = mpsc::channel();
        std::thread::spawn(move || relay(stdin, prelude, feed, messages));
        Ok(Started { child, channel })
    }

    /// On this computer: the script on bash's stdin, which dies with this process, or, where
    /// nothing signals a parent's death or a hold needs the channel, from a file.
    fn here(values: &CallValues, half: &str, hold: bool) -> io::Result<(Command, String, Feed)> {
        if !hold && cfg!(target_os = "linux") {
            let watch = Watch {
                off: true,
                hold: false,
                lease: 0,
            };
            let mut bash = Command::new("bash");
            bash.arg("-s");
            return Ok((bash, values.script(watch, half), Feed::Close));
        }
        let watch = Watch {
            off: false,
            hold,
            lease: 0,
        };
        let file = ScriptFile::write(&values.script(watch, half))?;
        let mut bash = Command::new("bash");
        bash.arg(file);
        Ok((bash, String::new(), Feed::Quiet))
    }

    fn over_ssh(
        host: &str,
        values: &CallValues,
        half: &str,
        live: Liveness,
        hold: bool,
    ) -> (Command, String, Feed) {
        let watching = hold || !(live.no_live || live.no_watchdog);
        let lease = if watching { live.lease } else { 0 };
        let watch = Watch {
            off: !watching,
            hold,
            lease,
        };
        let payload = encode(&values.script(watch, half));
        let feed = match (live.no_live && !hold, lease) {
            (true, _) => Feed::Stdin,
            (false, 0) => Feed::Quiet,
            (false, lease) => Feed::Heartbeat(Duration::from_millis(lease * 250)),
        };
        let mut ssh = Command::new("ssh");
        ssh.args(Ssh::options())
            .arg(host)
            .arg(Ssh::far_line(payload.len()));
        (ssh, payload, feed)
    }
}

/// How dibs runs ssh.
pub struct Ssh;

impl Ssh {
    pub fn connect_timeout() -> String {
        std::env::var("DIBS_CONNECT_TIMEOUT")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_CONNECT_TIMEOUT.into())
    }

    /// No TTY, so nothing downstream believes it is interactive, and a machine that stops
    /// answering is given up on after about two minutes.
    fn options() -> Vec<String> {
        [
            "BatchMode=yes".to_string(),
            "LogLevel=ERROR".into(),
            format!("ConnectTimeout={}", Ssh::connect_timeout()),
            "ServerAliveInterval=30".into(),
            "ServerAliveCountMax=4".into(),
        ]
        .into_iter()
        .flat_map(|o| ["-o".to_string(), o])
        .collect()
    }

    /// Where the script is written there; unexpanded, for the far shell's own `$HOME`.
    pub fn remote_dir() -> Option<String> {
        std::env::var("DIBS_REMOTE_DIR")
            .ok()
            .filter(|v| !v.is_empty())
    }

    /// The line the login shell there runs, which fish and bash read alike: the script is read
    /// by length off stdin, and what follows it is the channel.
    fn far_line(count: usize) -> String {
        let dir = Ssh::remote_dir().unwrap_or_else(|| "$HOME/.cache/dibs/run".into());
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let script = format!("{dir}/.dibs-payload.{}.{secs}.sh", std::process::id());
        let trace = match std::env::var("DIBS_TRACE") {
            Ok(v) if !v.is_empty() => "-x",
            _ => "",
        };
        format!(
            "mkdir -p {dir} 2>/dev/null; dd bs=1 count={count} 2>/dev/null | base64 -d | gzip -dc > {script} && {CONTINUATION}exec bash {trace} {script}\nexit 70"
        )
    }

    /// The address ssh dials for a host, which a held command reaches its services at.
    pub fn dials(host: &str) -> Option<String> {
        let out = Command::new("ssh")
            .args(["-G", host])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("hostname ").map(str::to_string))
            .filter(|h| !h.is_empty())
    }
}

/// Copies the script, then the channel, into the machine half's stdin, and closes it when told
/// to or when this process ends.
fn relay(mut stdin: ChildStdin, prelude: String, feed: Feed, messages: Receiver<Message>) {
    if stdin.write_all(prelude.as_bytes()).is_err() {
        return;
    }
    let beat = match feed {
        Feed::Close => return,
        Feed::Stdin => {
            let _ = io::copy(&mut io::stdin().lock(), &mut stdin);
            return;
        }
        Feed::Quiet => None,
        Feed::Heartbeat(every) => Some(every),
    };
    loop {
        let next = match beat {
            Some(every) => messages.recv_timeout(every),
            None => messages.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let line = match next {
            Ok(Message::Release(status)) => format!("release {status}\n"),
            Err(RecvTimeoutError::Timeout) => "\n".to_string(),
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if stdin.write_all(line.as_bytes()).is_err() {
            return;
        }
    }
}

/// The kernel signals the child when this process dies, SIGKILL included.
#[cfg(target_os = "linux")]
fn parent_death_signal() {
    // SAFETY: prctl with these arguments only sets a flag on the calling process.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) };
}

#[cfg(not(target_os = "linux"))]
fn parent_death_signal() {}

/// The exit a shell reports for a process: its code, or 128 and the signal that ended it.
pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or_default())
}

/// Ctrl-C while a child runs belongs to the child: this process carries on, and dies of it only
/// if the child did, as a shell does.
pub struct Interrupt {
    previous: libc::sighandler_t,
}

extern "C" fn noted(_: libc::c_int) {}

impl Interrupt {
    pub fn defer() -> Interrupt {
        let handler = noted as extern "C" fn(libc::c_int);
        // SAFETY: the handler does nothing, and is replaced again on drop.
        let previous = unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) };
        Interrupt { previous }
    }

    /// A child that died of Ctrl-C takes this process with it.
    pub fn pass_on(status: ExitStatus) {
        if status.signal() == Some(libc::SIGINT) {
            // SAFETY: restores the default action and raises it on this process.
            unsafe {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::raise(libc::SIGINT);
            }
        }
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        // SAFETY: puts back the handler `defer` replaced.
        unsafe { libc::signal(libc::SIGINT, self.previous) };
    }
}

/// A script in `TMPDIR`, which the machine half removes when it ends.
struct ScriptFile;

impl ScriptFile {
    fn write(script: &str) -> io::Result<PathBuf> {
        let dir = std::env::var_os("TMPDIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
            ^ u128::from(std::process::id()) << 64;
        let mut last = None;
        for attempt in 0..16u32 {
            let path = dir.join(format!(
                "dibs-hold-script.{}",
                suffix(seed.wrapping_add(u128::from(attempt) * 7919))
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(mut file) => {
                    file.write_all(script.as_bytes())?;
                    return Ok(path);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("no name left for a script")))
    }
}

fn suffix(mut n: u128) -> String {
    const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    (0..6)
        .map(|_| {
            let c = LETTERS[(n % LETTERS.len() as u128) as usize];
            n /= LETTERS.len() as u128;
            char::from(c)
        })
        .collect()
}
