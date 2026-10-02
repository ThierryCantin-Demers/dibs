//! `--hold`: the machine's lock is held by a job there that only waits, while the command runs
//! here with the terminal; the command is stopped if the lock goes first.

use crate::{
    cli::Command,
    machine::{Interrupt, Message, Reach, Started, exit_code},
};
use std::{
    io::{BufRead as _, BufReader, Write as _},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::process::CommandExt as _,
    },
    process::{Child, ChildStdout, ExitStatus},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

/// The line the machine prints once the lock is held, with each picked port after it.
const HOLDING: &str = "DIBS-HOLDING";

/// What runs here while the lock is held.
pub struct Hold<'a> {
    pub command: &'a Command,
    /// The lock as the notice names it: `shared` or `bench`.
    pub lock: &'a str,
    /// The machine as the notices name it.
    pub at: String,
    /// The lock's machine, lowercased, which a call inside the hold must not take again.
    pub lock_at: String,
    /// Where the command reaches the machine's services.
    pub reach: Reach,
}

#[derive(Default)]
struct Shared {
    command: Option<u32>,
    command_done: bool,
    holder_gone: bool,
}

enum Event {
    Line(String),
    Closed,
}

impl Hold<'_> {
    /// Runs the command once the machine holds the lock; the exit is the holder's.
    pub fn run(&self, started: Started) -> std::io::Result<i32> {
        let Started { mut child, channel } = started;
        let stdout = child.stdout.take().expect("a hold's stdout is piped");
        let lines = read_lines(stdout);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let at = self.at.clone();
        let watched = Arc::clone(&shared);
        let holder = std::thread::spawn(move || {
            let status = child.wait();
            let mut state = watched.lock().unwrap_or_else(|e| e.into_inner());
            state.holder_gone = true;
            if let Some(pid) = state.command.filter(|_| !state.command_done) {
                stop_early(&at, pid);
            }
            status
        });

        let mut ports = None;
        for event in lines.iter() {
            match event {
                Event::Line(line) if line == HOLDING || line.starts_with("DIBS-HOLDING ") => {
                    ports = Some(line[HOLDING.len()..].to_string());
                    break;
                }
                Event::Line(line) => println!("{line}"),
                Event::Closed => break,
            }
        }
        let Some(ports) = ports else {
            drop(channel);
            return Ok(holder_exit(holder.join()));
        };

        eprintln!(
            "dibs: holding the {} lock on {}, running here: {}",
            self.lock,
            self.at,
            self.command.shell_string()
        );
        let interrupt = Interrupt::defer();
        let status = match self.guard(&ports).and_then(Guard::spawn) {
            Ok((mut running, _liveness)) => {
                {
                    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                    state.command = Some(running.id());
                    if state.holder_gone {
                        stop_early(&self.at, running.id());
                    }
                }
                running.wait().map(exit_code).unwrap_or(1)
            }
            Err(e) => {
                eprintln!("dibs: could not run {}: {e}", self.command.shell_string());
                127
            }
        };
        drop(interrupt);
        shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .command_done = true;
        let _ = channel.send(Message::Release(status));
        drop(channel);
        let mut out = std::io::stdout().lock();
        for event in lines.iter() {
            match event {
                Event::Line(line) => {
                    let _ = writeln!(out, "{line}");
                }
                Event::Closed => break,
            }
        }
        Ok(holder_exit(holder.join()))
    }

    /// The guard that runs the command, with the machine's ports and the hold it runs inside in
    /// its environment.
    fn guard(&self, ports: &str) -> std::io::Result<std::process::Command> {
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .arg(Guard::WORD)
            .arg(&self.at)
            .args(self.command.words());
        let holding = match std::env::var("DIBS_HOLDING") {
            Ok(outer) if !outer.is_empty() => format!("{outer} {}", self.lock_at),
            _ => self.lock_at.clone(),
        };
        command.env("DIBS_HOLDING", holding);
        let picked: Vec<(&str, &str)> = ports
            .split_whitespace()
            .filter_map(|p| p.split_once('='))
            .collect();
        if !picked.is_empty() {
            let reach = self.reach.address();
            for (name, port) in picked {
                let name = name.to_ascii_uppercase();
                command.env(format!("DIBS_PORT_{name}"), port);
                command.env(format!("DIBS_SERVICE_{name}"), format!("{reach}:{port}"));
            }
        }
        if let Some(path) = std::env::var_os("DIBS_CALLER_PATH").filter(|p| !p.is_empty()) {
            command.env("PATH", path).env_remove("DIBS_CALLER_PATH");
        }
        Ok(command)
    }
}

/// Runs a held command in the terminal's process group, and stops it and everything under it
/// when the dibs holding its lock dies, however it dies.
pub struct Guard;

impl Guard {
    /// The word a guard is started with, outside the grammar.
    pub const WORD: &'static str = "__hold-guard";
    /// Where the guard reads the end of its dibs: the pipe only that dibs writes to.
    const LIVENESS: libc::c_int = 3;

    fn spawn(mut guard: std::process::Command) -> std::io::Result<(Child, OwnedFd)> {
        let mut fds = [0; 2];
        // SAFETY: pipe fills the two-element array it is given.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both descriptors were just created, and nothing else owns them.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        close_on_exec(read.as_raw_fd(), true);
        close_on_exec(write.as_raw_fd(), true);
        let raw = read.as_raw_fd();
        // SAFETY: dup2 and fcntl are async-signal-safe.
        unsafe {
            guard.pre_exec(move || {
                match raw == Guard::LIVENESS {
                    true => close_on_exec(raw, false),
                    false if libc::dup2(raw, Guard::LIVENESS) < 0 => {
                        return Err(std::io::Error::last_os_error());
                    }
                    false => {}
                }
                Ok(())
            });
        }
        let child = guard.spawn()?;
        drop(read);
        Ok((child, write))
    }

    /// The guard's own side: runs the command, and stops it if its dibs goes first.
    pub fn serve(at: &str, words: &[String]) -> i32 {
        close_on_exec(Guard::LIVENESS, true);
        let mut command = match words {
            [one] => {
                let mut bash = std::process::Command::new("bash");
                bash.args(["-c", one]);
                bash
            }
            [first, rest @ ..] => {
                let mut direct = std::process::Command::new(first);
                direct.args(rest);
                direct
            }
            [] => std::process::Command::new("true"),
        };
        let interrupt = Interrupt::defer();
        let mut running = match command.spawn() {
            Ok(running) => running,
            Err(e) => {
                eprintln!("dibs: could not run {}: {e}", words.join(" "));
                return 127;
            }
        };
        let pid = running.id();
        let done = Arc::new(AtomicBool::new(false));
        let watched = Arc::clone(&done);
        let at = at.to_string();
        std::thread::spawn(move || {
            // SAFETY: the guard was started with its dibs's pipe here, and nothing else uses it.
            let mut liveness = unsafe { std::fs::File::from_raw_fd(Guard::LIVENESS) };
            let _ = std::io::copy(&mut liveness, &mut std::io::sink());
            if !watched.load(Ordering::SeqCst) {
                stop_early(&at, pid);
            }
        });
        let status = running.wait().map(exit_code).unwrap_or(1);
        drop(interrupt);
        done.store(true, Ordering::SeqCst);
        status
    }
}

fn close_on_exec(fd: libc::c_int, on: bool) {
    // SAFETY: fcntl on a descriptor this process holds changes only its flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            let flags = match on {
                true => flags | libc::FD_CLOEXEC,
                false => flags & !libc::FD_CLOEXEC,
            };
            libc::fcntl(fd, libc::F_SETFD, flags);
        }
    }
}

/// The holder's stdout, a line at a time, read on a thread of its own so the machine never
/// waits on this side.
fn read_lines(stdout: ChildStdout) -> mpsc::Receiver<Event> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if send.send(Event::Line(line)).is_err() {
                return;
            }
        }
        let _ = send.send(Event::Closed);
    });
    receive
}

fn holder_exit(joined: std::thread::Result<std::io::Result<ExitStatus>>) -> i32 {
    match joined {
        Ok(Ok(status)) => exit_code(status),
        _ => 1,
    }
}

/// The lock went first, so the command would go on unlocked: it and everything under it stop.
fn stop_early(at: &str, pid: u32) {
    eprintln!("dibs: the lock on {at} ended before the command did, so the command was stopped.");
    for p in tree(pid) {
        // SAFETY: a plain signal to a process found under the command.
        unsafe { libc::kill(p as libc::pid_t, libc::SIGTERM) };
    }
}

/// A process and everything below it.
fn tree(pid: u32) -> Vec<u32> {
    let children = std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    std::iter::once(pid)
        .chain(
            children
                .split_whitespace()
                .filter_map(|c| c.parse().ok())
                .flat_map(tree),
        )
        .collect()
}
