use crate::{
    job::{environment::Environment, tether::Tether},
    sink::Sink,
    stop::Signals,
};
use dibs_format::Exit;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write as _},
    os::{
        fd::{AsRawFd as _, BorrowedFd},
        unix::process::CommandExt as _,
    },
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

/// The status a job that overran its cap ends with, as `timeout` gives it.
const KILLED: i32 = 128 + libc::SIGKILL;

/// Where a job's output goes.
#[derive(Debug, Clone, Copy)]
pub enum Output<'a> {
    /// Into its log, both streams in the order they were written.
    Log(&'a Path),
    /// Into its log and to the caller as it is written.
    Stream(&'a Path),
    /// To the caller alone, each stream as itself.
    Caller,
    /// The runner's own three streams, a transfer's: stdin included, since rsync talks both ways.
    Through,
}

impl Output<'_> {
    /// Says what dibs has to say about the job where the job's own output goes.
    pub fn tell(self, sink: &Sink, text: &str) {
        if text.is_empty() {
            return;
        }
        let logged = |log: &Path| {
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log) {
                let _ = file.write_all(text.as_bytes());
            }
        };
        match self {
            Output::Log(log) => logged(log),
            Output::Stream(log) => {
                logged(log);
                sink.out(text.as_bytes());
            }
            Output::Caller | Output::Through => sink.say(text),
        }
    }
}

/// How long a job may run, and how long it is given to stop once told to.
#[derive(Debug, Clone, Copy)]
pub struct Cap {
    pub after: Duration,
    pub grace: Duration,
}

/// How a job ended: its status as a shell gives it, and whether its cap stopped it. A command
/// that runs `timeout` itself can end 124 on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobEnd {
    pub status: i32,
    pub capped: bool,
}

/// A job started: `bash -c` the command, in a process group of its own, stdin from `/dev/null`.
pub struct Job {
    pub pid: u32,
    exited: mpsc::Receiver<io::Result<ExitStatus>>,
    relays: Vec<JoinHandle<()>>,
    /// None only where its sweeper could not be started.
    tether: Option<Tether>,
}

impl Job {
    /// `holds` is a lock the job's own processes hold too, so it stays held while any of them
    /// lives, whatever becomes of this runner.
    pub fn spawn(
        command: &str,
        environment: &Environment,
        output: Output,
        sink: &Sink,
        holds: Option<BorrowedFd>,
    ) -> io::Result<Job> {
        let mut bash = Command::new("bash");
        bash.arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .process_group(0);
        Signals::unblocked(&mut bash);
        environment.apply(&mut bash);
        if let Some(fd) = holds.map(|fd| fd.as_raw_fd()) {
            // SAFETY: fcntl is async-signal-safe, on a descriptor open until exec.
            unsafe {
                bash.pre_exec(move || match libc::fcntl(fd, libc::F_SETFD, 0) {
                    -1 => Err(io::Error::last_os_error()),
                    _ => Ok(()),
                });
            }
        }
        let tether = Tether::start().ok();
        if let Some(tether) = &tether {
            tether.tie(&mut bash);
        }
        let mut relays = Vec::new();
        let child = match output {
            Output::Through => bash.stdin(Stdio::inherit()).spawn()?,
            Output::Log(log) => {
                let file = Job::log_file(log)?;
                bash.stderr(file.try_clone()?).stdout(file).spawn()?
            }
            Output::Stream(log) => {
                let (reader, writer) = io::pipe()?;
                bash.stderr(writer.try_clone()?).stdout(writer);
                let child = bash.spawn()?;
                let mut file = Job::log_file(log)?;
                let sink = sink.clone();
                relays.push(Job::relay(reader, move |bytes| {
                    let _ = file.write_all(bytes);
                    sink.out(bytes);
                }));
                child
            }
            Output::Caller => {
                let mut child = bash.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
                if let Some(out) = child.stdout.take() {
                    let sink = sink.clone();
                    relays.push(Job::relay(out, move |bytes| {
                        sink.out(bytes);
                    }));
                }
                if let Some(err) = child.stderr.take() {
                    let sink = sink.clone();
                    relays.push(Job::relay(err, move |bytes| sink.err(bytes)));
                }
                child
            }
        };
        // It holds this side's copy of the pipe, whose end is what ends the relay.
        drop(bash);
        Ok(Job::watched(child, relays, tether))
    }

    /// Appended to, since what laid out the job's tree wrote there first.
    fn log_file(path: &Path) -> io::Result<File> {
        OpenOptions::new().create(true).append(true).open(path)
    }

    fn relay(
        mut from: impl Read + Send + 'static,
        mut to: impl FnMut(&[u8]) + Send + 'static,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n @ 1..) = from.read(&mut chunk) {
                to(&chunk[..n]);
            }
        })
    }

    fn watched(mut child: Child, relays: Vec<JoinHandle<()>>, tether: Option<Tether>) -> Job {
        let pid = child.id();
        let (tell, exited) = mpsc::channel();
        thread::spawn(move || {
            let _ = tell.send(child.wait());
        });
        Job {
            pid,
            exited,
            relays,
            tether,
        }
    }

    /// Waits for the job, stopping its group when it overruns the cap, and says how it ended.
    pub fn wait(self, cap: Option<Cap>) -> JobEnd {
        let status = match cap {
            None => self.exited.recv().ok(),
            Some(cap) => match self.exited.recv_timeout(cap.after) {
                Ok(status) => Some(status),
                Err(_) => {
                    self.signal_group(libc::SIGTERM);
                    let status = match self.exited.recv_timeout(cap.grace) {
                        Ok(_) => Exit::Overran.status(),
                        Err(_) => {
                            self.signal_group(libc::SIGKILL);
                            let _ = self.exited.recv();
                            KILLED
                        }
                    };
                    self.end();
                    return JobEnd {
                        status,
                        capped: true,
                    };
                }
            },
        };
        self.end();
        JobEnd {
            status: match status {
                Some(Ok(status)) => Exit::shell_status(status),
                _ => Exit::Failed.status(),
            },
            capped: false,
        }
    }

    /// Waits for the job on a thread of its own, which hands its status on; the pid comes back.
    pub fn on_end(self, then: impl FnOnce(i32) + Send + 'static) -> u32 {
        let pid = self.pid;
        thread::spawn(move || then(self.wait(None).status));
        pid
    }

    fn signal_group(&self, signal: libc::c_int) {
        // SAFETY: kill only sends a signal, here to the job's own group.
        unsafe { libc::kill(-(self.pid as libc::pid_t), signal) };
    }

    /// Whatever the job left running in its group goes before its output is taken as finished,
    /// since what is left may hold the output open, and before the lock goes.
    fn end(self) {
        if let Some(tether) = self.tether {
            tether.sweep();
        }
        for relay in self.relays {
            let _ = relay.join();
        }
    }
}
