//! Starting a submission's worker so it outlives the call that started it: a systemd user unit
//! where the account has a user manager, a session of its own otherwise. `DIBS_ORCH_SPAWN`
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
    "DIBS_ORCH_HOME",
    "DIBS_ORCH_DIBS",
    "DIBS_MACHINES",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "DIBS_LOCAL",
    "DIBS_ROOT",
    "DIBS_ORCH_RECHECK",
];

pub fn started_fifo(home: &Home, id: &str) -> PathBuf {
    home.submission(id).join("started")
}

/// Starts the worker of `id` and waits for it to hold the submission, or to find another
/// worker holding it. Err says why it did not start.
pub fn worker(home: &Home, id: &str) -> Result<(), String> {
    let dir = home.submission(id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Two starters at once would share one fifo, and one would wait out a reply the other read.
    let _starting = File::create(dir.join("starting.lock"))
        .and_then(|f| f.lock().map(|()| f))
        .map_err(|e| format!("{}: {e}", dir.join("starting.lock").display()))?;
    if crate::worker::driven(home, id) {
        return Ok(());
    }
    let fifo = started_fifo(home, id);
    let _ = std::fs::remove_file(&fifo);
    mkfifo(&fifo).map_err(|e| format!("{}: {e}", fifo.display()))?;
    let (told, said) = mpsc::channel();
    {
        let fifo = fifo.clone();
        std::thread::spawn(move || {
            let line = File::open(&fifo).ok().and_then(|f| {
                let mut line = String::new();
                BufReader::new(f).read_line(&mut line).ok().map(|_| line)
            });
            let _ = told.send(line.unwrap_or_default());
        });
    }
    launch(id, &dir)?;
    match said.recv_timeout(STARTED_WITHIN) {
        Ok(line) if [DRIVING, DRIVEN].contains(&line.trim()) => Ok(()),
        Ok(line) => Err(format!(
            "its worker ended before it held the submission{}; see {}",
            match line.trim() {
                "" => String::new(),
                said => format!(" ({said})"),
            },
            dir.join("worker.log").display()
        )),
        Err(_) => {
            // Unblocks the reader, which would otherwise wait on the fifo for ever.
            let _ = File::options()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo);
            Err(format!(
                "its worker did not say it held the submission within {}s; see {}",
                STARTED_WITHIN.as_secs(),
                dir.join("worker.log").display()
            ))
        }
    }
}

fn launch(id: &str, dir: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let log = File::options()
        .create(true)
        .append(true)
        .open(dir.join("worker.log"))
        .map_err(|e| e.to_string())?;
    let systemd = match std::env::var("DIBS_ORCH_SPAWN").ok().as_deref() {
        Some("setsid") => false,
        Some("systemd") => true,
        _ => std::env::var_os("XDG_RUNTIME_DIR").is_some() && on_path("systemd-run"),
    };
    let mut command = match systemd {
        true => {
            let mut run = Command::new("systemd-run");
            run.args(["--user", "--quiet", "--collect"])
                .arg(format!("--unit=dibs-orch-{id}-{}", std::process::id()))
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
    if systemd {
        let status = child.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("systemd-run failed ({status})"));
        }
    } else {
        // Reaped by a thread, so it never lingers as a zombie while this call lives.
        std::thread::spawn(move || child.wait());
    }
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
