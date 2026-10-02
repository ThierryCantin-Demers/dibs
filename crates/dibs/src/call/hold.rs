//! `--hold`: the machine's lock is held by a job there that only waits, while the command runs
//! here with the terminal; the command is stopped if the lock goes first.

use crate::{
    cli::{Command, Service},
    machine::{Interrupt, Message, Reach, Started, exit_code},
};
use dibs_format::Exit;
use std::{
    fmt,
    io::{BufRead as _, BufReader, ErrorKind, Write as _},
    net::{TcpStream, ToSocketAddrs as _},
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
    time::{Duration, Instant},
};

/// How long a connection to a ready service may take, its name's lookup included.
const REACH_WITHIN: Duration = Duration::from_secs(3);
/// How long a name may take to resolve before a connection that timed out is put down to it.
const RESOLVE_WITHIN: Duration = Duration::from_secs(2);

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
    /// The services the machine runs for the call, which the command here has to reach.
    pub services: &'a [Service],
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

        if let Some(unreached) = self.unreached(&ports) {
            eprintln!("{unreached}");
            return Ok(self.release(
                i32::from(Exit::ServiceFailed.code()),
                &shared,
                channel,
                &lines,
                holder,
            ));
        }
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
        Ok(self.release(status, &shared, channel, &lines, holder))
    }

    /// Releases the lock with the command's status, passes on what the machine says after it,
    /// and gives the holder's exit.
    fn release(
        &self,
        status: i32,
        shared: &Mutex<Shared>,
        channel: mpsc::Sender<Message>,
        lines: &mpsc::Receiver<Event>,
        holder: std::thread::JoinHandle<std::io::Result<ExitStatus>>,
    ) -> i32 {
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
        holder_exit(holder.join())
    }

    /// The first `--ready tcp:` service this computer cannot connect to, which the command here
    /// would fail at: ready on the machine, but behind its firewall or on its loopback only.
    fn unreached(&self, ports: &str) -> Option<Unreached> {
        let picked: Vec<(&str, &str)> = ports
            .split_whitespace()
            .filter_map(|p| p.split_once('='))
            .collect();
        let mut host = None;
        for service in self.services {
            let Some(ready) = service
                .ready
                .as_deref()
                .and_then(|r| r.strip_prefix("tcp:"))
            else {
                continue;
            };
            let named = ready.rsplit(':').next().unwrap_or_default();
            let port = picked
                .iter()
                .find(|(name, _)| *name == named)
                .map_or(named, |(_, port)| port);
            let Ok(port) = port.parse::<u16>() else {
                continue;
            };
            let host: &String = host.get_or_insert_with(|| {
                let address = self.reach.address();
                match address.split_once('@') {
                    Some((_, host)) => host.to_string(),
                    None => address,
                }
            });
            let cause = match Attempt::connect(host, port) {
                Attempt::Refused => Cause::Refused,
                Attempt::NoRoute => Cause::Rejected,
                Attempt::TimedOut if Attempt::resolves(host) => Cause::Dropped,
                Attempt::Connected | Attempt::TimedOut | Attempt::Failed => continue,
            };
            return Some(Unreached {
                service: service.name.0.clone(),
                at: self.at.clone(),
                host: host.clone(),
                port,
                cause,
            });
        }
        None
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

/// How a connection from here to a ready service went.
enum Attempt {
    Connected,
    Refused,
    NoRoute,
    TimedOut,
    /// Anything else, which says nothing about the machine's firewall.
    Failed,
}

impl Attempt {
    /// One connection, its name's lookup included, within `REACH_WITHIN`.
    fn connect(host: &str, port: u16) -> Attempt {
        let (tell, heard) = mpsc::channel();
        let host = host.to_string();
        std::thread::spawn(move || {
            let deadline = Instant::now() + REACH_WITHIN;
            let Ok(addresses) = (host.as_str(), port).to_socket_addrs() else {
                let _ = tell.send(Attempt::Failed);
                return;
            };
            let mut last = Attempt::Failed;
            for address in addresses {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                last = match TcpStream::connect_timeout(&address, left) {
                    Ok(_) => Attempt::Connected,
                    Err(e) => match e.kind() {
                        ErrorKind::ConnectionRefused => Attempt::Refused,
                        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable => {
                            Attempt::NoRoute
                        }
                        ErrorKind::TimedOut => Attempt::TimedOut,
                        _ => Attempt::Failed,
                    },
                };
                if matches!(last, Attempt::Connected) {
                    break;
                }
            }
            let _ = tell.send(last);
        });
        heard
            .recv_timeout(REACH_WITHIN)
            .unwrap_or(Attempt::TimedOut)
    }

    /// Whether the name resolves quickly, without which a timeout may be a slow lookup and says
    /// nothing about the port.
    fn resolves(host: &str) -> bool {
        let (tell, heard) = mpsc::channel();
        let host = host.to_string();
        std::thread::spawn(move || {
            let _ = tell.send((host.as_str(), 0).to_socket_addrs().is_ok());
        });
        heard.recv_timeout(RESOLVE_WITHIN).unwrap_or(false)
    }
}

/// What a connection that failed most likely ran into.
enum Cause {
    /// Nothing listens there from outside: a loopback-only server, or a reset.
    Refused,
    Rejected,
    Dropped,
}

/// A ready service this computer cannot connect to.
struct Unreached {
    service: String,
    at: String,
    host: String,
    port: u16,
    cause: Cause,
}

impl fmt::Display for Unreached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Unreached {
            service,
            at,
            host,
            port,
            cause,
        } = self;
        let why = match cause {
            Cause::Refused => "nothing answers there from outside the machine: the server listens on its loopback only, or a firewall resets the connection".to_string(),
            Cause::Rejected => format!("a firewall on {at} most likely rejects it"),
            Cause::Dropped => format!("a firewall on {at} most likely drops it"),
        };
        write!(
            f,
            "dibs: {service} is ready on {at}, but this computer cannot connect to {host}:{port}, so the command was not run: {why}. dibs with --there runs the command on the machine instead."
        )
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
