//! Starting a submission's worker so it outlives the call that started it: a systemd user unit
//! where the account has a user manager, a session of its own otherwise. `DIBSPATCHER_SPAWN`
//! picks one (`systemd` or `setsid`).

use crate::home::Home;
use std::{
    ffi::CString,
    fs::File,
    io::{BufRead as _, BufReader},
    os::unix::{ffi::OsStrExt as _, fs::OpenOptionsExt as _, process::CommandExt as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

/// How long a start waits for its worker to say it holds the submission.
const STARTED_WITHIN: Duration = Duration::from_secs(30);

/// What a worker writes to its submission's `started` fifo.
pub const DRIVING: &str = "driving";
pub const DRIVEN: &str = "driven";

/// Variables a worker started by systemd gets from the call that started it, which a unit does
/// not inherit.
const PASSED: [&str; 10] = [
    "HOME",
    "PATH",
    "DIBSPATCHER_HOME",
    "DIBSPATCHER_DIBS",
    "DIBS_MACHINES",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "DIBS_LOCAL",
    "DIBS_ROOT",
    "DIBSPATCHER_RECHECK",
];

pub fn started_fifo(home: &Home, id: &str) -> PathBuf {
    home.submission(id).join("started")
}

/// Starts the worker of `id`, unless it has `ended` or another worker holds it, and waits for
/// the new one to hold the submission. Ok says whether this call started it; Err says why it
/// did not start.
pub fn worker(home: &Home, id: &str, ended: impl Fn() -> bool) -> Result<bool, String> {
    let dir = home.submission(id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _starting = starting(&dir)?;
    // A worker records its end before it lets go, so driven goes first.
    if crate::worker::driven(home, id) || ended() {
        return Ok(false);
    }
    let fifo = started_fifo(home, id);
    let _ = std::fs::remove_file(&fifo);
    mkfifo(&fifo).map_err(|e| format!("{}: {e}", fifo.display()))?;
    let (told, heard) = mpsc::channel();
    {
        let fifo = fifo.clone();
        let told = told.clone();
        std::thread::spawn(move || {
            let line = File::open(&fifo).ok().and_then(|f| {
                let mut line = String::new();
                BufReader::new(f).read_line(&mut line).ok().map(|_| line)
            });
            let _ = told.send(Heard::Said(line.unwrap_or_default()));
        });
    }
    // Unblocks the reader, which would otherwise wait on the fifo for ever.
    let unblock = || {
        let _ = File::options()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo);
    };
    if let Err(why) = launch(id, &dir, told) {
        unblock();
        return Err(why);
    }
    match heard.recv_timeout(STARTED_WITHIN) {
        Ok(Heard::Said(line)) if line.trim() == DRIVING => Ok(true),
        Ok(Heard::Said(line)) if line.trim() == DRIVEN => Ok(false),
        Ok(Heard::Failed(why)) => {
            unblock();
            Err(why)
        }
        Ok(Heard::Said(line)) => Err(format!(
            "its worker ended before it held the submission{}; see {}",
            match line.trim() {
                "" => String::new(),
                said => format!(" ({said})"),
            },
            dir.join("worker.log").display()
        )),
        Err(_) => {
            unblock();
            Err(format!(
                "its worker did not say it held the submission within {}s; see {}",
                STARTED_WITHIN.as_secs(),
                dir.join("worker.log").display()
            ))
        }
    }
}

/// Held by one start at a time: two at once would share one fifo, and one would wait out a
/// reply the other read.
fn starting(dir: &Path) -> Result<File, String> {
    let path = dir.join("starting.lock");
    let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let waiter = file
        .try_clone()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let (took, taken) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = took.send(waiter.lock());
    });
    match taken.recv_timeout(STARTED_WITHIN) {
        Ok(Ok(())) => Ok(file),
        Ok(Err(e)) => Err(format!("{}: {e}", path.display())),
        Err(_) => Err(format!(
            "another start held it for {}s",
            STARTED_WITHIN.as_secs()
        )),
    }
}

/// What a start hears first: its worker on the fifo, or the launcher failing.
enum Heard {
    Said(String),
    Failed(String),
}

/// Starts the worker. A launcher that fails says so on `told`, and one that never returns is
/// left to the start's own wait.
fn launch(id: &str, dir: &Path, told: mpsc::Sender<Heard>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let log = File::options()
        .create(true)
        .append(true)
        .open(dir.join("worker.log"))
        .map_err(|e| e.to_string())?;
    let systemd = match std::env::var("DIBSPATCHER_SPAWN").ok().as_deref() {
        Some("setsid") => false,
        Some("systemd") => true,
        _ => std::env::var_os("XDG_RUNTIME_DIR").is_some() && on_path("systemd-run"),
    };
    let mut command = match systemd {
        true => {
            let mut run = Command::new("systemd-run");
            run.args(["--user", "--quiet", "--collect"])
                .arg(format!("--unit=dibspatcher-{id}-{}", std::process::id()))
                .arg("--property=KillMode=control-group");
            for name in PASSED {
                if std::env::var_os(name).is_some() {
                    run.arg(format!("--setenv={name}"));
                }
            }
            run.arg(format!(
                "--property=StandardOutput=append:{}",
                dir.join("worker.log").display()
            ))
            .arg(format!(
                "--property=StandardError=append:{}",
                dir.join("worker.log").display()
            ))
            .arg(&exe)
            .args(["worker", id]);
            run
        }
        false => {
            let mut worker = Command::new(&exe);
            worker.args(["worker", id]);
            // SAFETY: setsid is async-signal-safe.
            unsafe {
                worker.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
            worker
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    let mut child = command
        .spawn()
        .map_err(|e| format!("{}: {e}", command.get_program().to_string_lossy()))?;
    // Reaped by a thread, so it never lingers as a zombie while this call lives.
    std::thread::spawn(move || {
        let status = child.wait();
        if systemd && !status.as_ref().is_ok_and(|s| s.success()) {
            let why = match status {
                Ok(status) => format!("systemd-run failed ({status})"),
                Err(e) => format!("systemd-run: {e}"),
            };
            let _ = told.send(Heard::Failed(why));
        }
    });
    Ok(())
}

/// Tells a start waiting on `started` what this worker found, if one waits.
pub fn tell(fifo: &Path, word: &str) {
    if let Ok(mut f) = File::options()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(fifo)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{word}");
    }
}

fn mkfifo(path: &Path) -> std::io::Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: c is a valid NUL-terminated path.
    match unsafe { libc::mkfifo(c.as_ptr(), 0o600) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}
