use crate::{job::environment::Environment, sink::Sink, stop::Signals};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write as _},
    os::unix::process::{CommandExt as _, ExitStatusExt as _},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

/// The status a job that overran its cap ends with, as `timeout` gives it.
const OVERRAN: i32 = 124;
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
}

/// How long a job may run, and how long it is given to stop once told to.
#[derive(Debug, Clone, Copy)]
pub struct Cap {
    pub after: Duration,
    pub grace: Duration,
}

/// A job started: `bash -c` the command, in a process group of its own, stdin from `/dev/null`.
pub struct Job {
    pub pid: u32,
    exited: mpsc::Receiver<io::Result<ExitStatus>>,
    relays: Vec<JoinHandle<()>>,
}

impl Job {
    pub fn spawn(
        command: &str,
        environment: &Environment,
        output: Output,
        sink: &Sink,
    ) -> io::Result<Job> {
        let mut bash = Command::new("bash");
        bash.arg("-c")
            .arg(command)
            .stdin(Stdio::null())
            .process_group(0);
        Signals::unblocked(&mut bash);
        environment.apply(&mut bash);
        let mut relays = Vec::new();
        let child = match output {
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
                    relays.push(Job::relay(out, move |bytes| sink.out(bytes)));
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
        Ok(Job::watched(child, relays))
    }

    fn log_file(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
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

    fn watched(mut child: Child, relays: Vec<JoinHandle<()>>) -> Job {
        let pid = child.id();
        let (tell, exited) = mpsc::channel();
        thread::spawn(move || {
            let _ = tell.send(child.wait());
        });
        Job {
            pid,
            exited,
            relays,
        }
    }

    /// Waits for the job, stopping its group when it overruns the cap, and returns its status as a
    /// shell gives it.
    pub fn wait(self, cap: Option<Cap>) -> i32 {
        let status = match cap {
            None => self.exited.recv().ok(),
            Some(cap) => match self.exited.recv_timeout(cap.after) {
                Ok(status) => Some(status),
                Err(_) => {
                    self.signal_group(libc::SIGTERM);
                    let status = match self.exited.recv_timeout(cap.grace) {
                        Ok(_) => OVERRAN,
                        Err(_) => {
                            self.signal_group(libc::SIGKILL);
                            let _ = self.exited.recv();
                            KILLED
                        }
                    };
                    self.join_relays();
                    return status;
                }
            },
        };
        self.join_relays();
        match status {
            Some(Ok(status)) => exit_code(status),
            _ => 1,
        }
    }

    fn signal_group(&self, signal: libc::c_int) {
        // SAFETY: kill only sends a signal, here to the job's own group.
        unsafe { libc::kill(-(self.pid as libc::pid_t), signal) };
    }

    fn join_relays(self) {
        for relay in self.relays {
            let _ = relay.join();
        }
    }
}

/// The exit a shell reports for a process: its code, or 128 and the signal that ended it.
pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or_default())
}
