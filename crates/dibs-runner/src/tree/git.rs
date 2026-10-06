use crate::{
    clock::Deadline,
    job::{Environment, reap},
    stop::Signals,
};
use std::{
    io,
    os::unix::process::CommandExt as _,
    path::Path,
    process::{Command, Output, Stdio},
    sync::mpsc,
    thread,
};

/// Runs what a stop must not cut short, such as the renames that replace a tree.
pub type Unstopped<'a> = &'a dyn Fn(&mut dyn FnMut());

/// How a prepare runs what it needs: in the job's environment, each in a process group of its
/// own, named to whatever stops the call while it runs, so that a stop takes it too, and stopped
/// at the job's cap, which the prepare counts against.
pub struct Commands<'a> {
    pub environment: &'a Environment,
    pub running: &'a dyn Fn(Option<u32>),
    pub unstopped: Unstopped<'a>,
    pub deadline: Deadline,
}

impl Commands<'_> {
    pub fn output(&self, mut command: Command) -> io::Result<Output> {
        if self.deadline.passed() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.environment.apply(&mut command);
        Signals::unblocked(&mut command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command.spawn()?;
        let pid = child.id();
        (self.running)(Some(pid));
        let output = match self.deadline.left() {
            None => child.wait_with_output(),
            Some(left) => {
                let (tell, ended) = mpsc::channel();
                thread::spawn(move || tell.send(child.wait_with_output()));
                ended.recv_timeout(left).unwrap_or_else(|_| {
                    reap(&[pid]);
                    Err(io::ErrorKind::TimedOut.into())
                })
            }
        };
        (self.running)(None);
        output
    }

    /// `git -C <dir> <args>`.
    pub fn git(&self, dir: &Path, args: &[&str]) -> io::Result<Output> {
        let mut git = Git(dir).command();
        git.args(args);
        self.output(git)
    }
}

/// git, run in one directory.
pub struct Git<'a>(pub &'a Path);

impl Git<'_> {
    /// `git -C <dir>`, for the arguments after it.
    pub fn command(&self) -> Command {
        let mut git = Command::new("git");
        git.arg("-C").arg(self.0);
        git
    }
}

/// What a command printed on stdout, trimmed, when it succeeded.
pub fn answer(output: io::Result<Output>) -> Option<String> {
    let output = output.ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Both of a command's streams, as `2>&1` would have caught them.
pub fn said(output: &io::Result<Output>) -> String {
    match output {
        Ok(o) => format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => e.to_string(),
    }
}
