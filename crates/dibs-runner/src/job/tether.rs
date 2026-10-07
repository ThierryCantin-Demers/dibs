use crate::{itself::Itself, stop::Signals};
use std::{
    io::{self, PipeWriter, Read as _},
    os::{fd::AsRawFd as _, unix::process::CommandExt as _},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

const ROUND: Duration = Duration::from_millis(250);
/// Rounds of the group's TERM before its KILL, and of its KILL before giving up on it.
const TERM_ROUNDS: usize = 40;
const KILL_ROUNDS: usize = 8;

/// What ties a job's process group to its runner: a process of its own, outside both, that sweeps
/// the group once the runner's end of a pipe closes. The runner closes it when the job has
/// ended, and a runner killed outright, by `KILL` or for memory, closes it too.
pub struct Tether {
    runner_end: Option<PipeWriter>,
    sweeper: Child,
}

/// A process group, by the pid of its leader.
#[derive(Debug, Clone, Copy)]
struct Group(pub u32);

impl Tether {
    pub const WORD: &str = "tether";

    /// Started before the job it ties, so no moment of the job's runs untied.
    pub fn start() -> io::Result<Tether> {
        let (sweeper_end, runner_end) = io::pipe()?;
        let mut command = Itself::command();
        command
            .arg(Tether::WORD)
            .stdin(sweeper_end)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        Signals::unblocked(&mut command);
        let sweeper = command.spawn()?;
        Ok(Tether {
            runner_end: Some(runner_end),
            sweeper,
        })
    }

    /// The job, in a group of its own, names that group to the tether between its fork and its
    /// exec, so a runner killed at any moment after the fork leaves it tied.
    pub fn tie(&self, job: &mut Command) {
        let Some(fd) = self.runner_end.as_ref().map(|end| end.as_raw_fd()) else {
            return;
        };
        // SAFETY: the closure makes async-signal-safe calls only, on a descriptor open until exec.
        unsafe {
            job.pre_exec(move || {
                let group = libc::getpid().to_ne_bytes();
                libc::write(fd, group.as_ptr().cast(), group.len());
                Ok(())
            });
        }
    }

    /// Sweeps what is left in the group, and returns once it is gone.
    pub fn sweep(mut self) {
        drop(self.runner_end.take());
        let _ = self.sweeper.wait();
    }

    /// `dibs-runner tether`: the group, then the end of stdin, which is the runner's end closing.
    pub fn serve() -> i32 {
        let mut stdin = io::stdin();
        let mut group = [0u8; size_of::<libc::pid_t>()];
        if stdin.read_exact(&mut group).is_err() {
            return 0;
        }
        let mut byte = [0u8; 1];
        while let Ok(1..) = stdin.read(&mut byte) {}
        Group(libc::pid_t::from_ne_bytes(group) as u32).sweep();
        0
    }
}

impl Group {
    /// TERM, then KILL for whatever outlives it, until nothing is left in it.
    fn sweep(self) {
        for round in 0..TERM_ROUNDS + KILL_ROUNDS {
            let signal = match round {
                0 => libc::SIGTERM,
                TERM_ROUNDS => libc::SIGKILL,
                _ => 0,
            };
            if !self.signal(signal) {
                return;
            }
            thread::sleep(ROUND);
        }
    }

    /// Whether anything in the group was there to receive it.
    fn signal(self, signal: libc::c_int) -> bool {
        // SAFETY: kill only sends a signal, here to a job's own group.
        unsafe { libc::kill(-(self.0 as libc::pid_t), signal) == 0 }
    }
}
