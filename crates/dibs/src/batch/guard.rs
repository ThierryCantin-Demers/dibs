//! A batch step's guard: it runs the step's line, and stops the step's whole process group the
//! moment the driver is gone, however the driver went.

use crate::{
    call::{BatchStep, CHECK_IN, Starter, WHEN_IDLE, Watched},
    itself::Itself,
};
use dibs_format::{Exit, MachineName};
use std::{
    io,
    os::unix::process::CommandExt as _,
    process::{Command, Stdio},
};

pub struct StepGuard;

/// What the batch was started with that every step it starts keeps, a follow's included.
pub struct Carried<'a> {
    pub check_in: Option<&'a str>,
    pub when_idle: bool,
}

impl StepGuard {
    /// The word a step's guard is started with, outside the grammar.
    pub const WORD: &str = "__step-guard";

    /// The driver's side: the guard of `line` started, leading a process group of its own, so a
    /// cancellation stops the step whole and a Ctrl-C at the driver's terminal reaches none of it.
    pub fn spawn(
        line: &str,
        batch: &BatchStep,
        on: Option<&MachineName>,
        carried: Carried,
    ) -> io::Result<Watched> {
        let mut guard = Itself::command();
        match on {
            Some(on) => guard.env("DIBS_ON", on.as_str()),
            None => guard.env_remove("DIBS_ON"),
        };
        match carried.check_in {
            Some(time) => guard.env(CHECK_IN, time),
            None => guard.env_remove(CHECK_IN),
        };
        match carried.when_idle {
            true => guard.env(WHEN_IDLE, "1"),
            false => guard.env_remove(WHEN_IDLE),
        };
        guard
            .args([StepGuard::WORD, line])
            .envs(batch.vars())
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Watched::spawn(guard)
    }

    /// The guard's side: the line through bash, as a shell would read it, its `dibs` this binary
    /// rather than whichever is first on the PATH, which may be another version.
    pub fn serve(line: &str) -> i32 {
        let starter = Starter::take();
        let script = format!("dibs() {{ \"$0\" \"$@\"; }}\n{line}");
        let mut step = match Command::new("bash")
            .arg("-c")
            .arg(script)
            .arg(Itself::path())
            .spawn()
        {
            Ok(step) => step,
            Err(e) => {
                eprintln!("dibs: could not run {line}: {e}");
                return 127;
            }
        };
        std::thread::spawn(move || {
            starter.gone();
            // SAFETY: signals this process's own group, which holds the step and nothing else.
            unsafe { libc::kill(0, libc::SIGTERM) };
        });
        step.wait().map(Exit::shell_status).unwrap_or(1)
    }
}
