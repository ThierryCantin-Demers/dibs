use crate::{job::Environment, stop::Signals};
use std::{
    io,
    os::unix::process::CommandExt as _,
    path::Path,
    process::{Command, Output, Stdio},
};

/// How a prepare runs what it needs: in the job's environment, each in a process group of its
/// own, named to whatever stops the call while it runs, so that a stop takes it too.
pub struct Commands<'a> {
    pub environment: &'a Environment,
    pub running: &'a dyn Fn(Option<u32>),
}

impl Commands<'_> {
    pub fn output(&self, mut command: Command) -> io::Result<Output> {
        self.environment.apply(&mut command);
        Signals::unblocked(&mut command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command.spawn()?;
        (self.running)(Some(child.id()));
        let output = child.wait_with_output();
        (self.running)(None);
        output
    }

    /// `git -C <dir> <args>`.
    pub fn git(&self, dir: &Path, args: &[&str]) -> io::Result<Output> {
        let mut git = Command::new("git");
        git.arg("-C").arg(dir).args(args);
        self.output(git)
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
