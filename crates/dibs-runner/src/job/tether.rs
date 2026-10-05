use crate::{itself::Itself, stop::Signals};
use std::{
    io::{self, PipeWriter, Read as _},
    os::unix::process::CommandExt as _,
    process::{Child, Stdio},
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
pub struct Group(pub u32);

impl Tether {
    pub const WORD: &str = "tether";

    pub fn to(group: Group) -> io::Result<Tether> {
        let (sweeper_end, runner_end) = io::pipe()?;
        let mut command = Itself::command();
        command
            .args([Tether::WORD, &group.0.to_string()])
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

    /// Sweeps what is left in the group, and returns once it is gone.
    pub fn sweep(mut self) {
        drop(self.runner_end.take());
        let _ = self.sweeper.wait();
    }

    /// `dibs-runner tether <group>`: waits for the runner's end of stdin to close, then sweeps.
    pub fn serve(group: &str) -> i32 {
        let Ok(group) = group.parse() else {
            eprintln!("dibs-runner: {group:?} is not a process group");
            return 2;
        };
        let mut byte = [0u8; 1];
        while let Ok(1..) = io::stdin().read(&mut byte) {}
        Group(group).sweep();
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
