//! Getting a machine with the right things held, which is the only thing the layer below is
//! for.
//!
//! Today that is `dibs`, a bash script that ships itself over ssh and takes an flock. It could
//! become `srun` against a Slurm cluster without anything above this file changing, which is
//! the entire reason the boundary is here: the crossover is the multi-device machine, and it
//! should be a swap rather than a rewrite.

use crate::recipe::{Isolation, Lock};
use std::process::{Command, Stdio};

pub struct Request<'a> {
    pub label: &'a str,
    pub lock: Lock,
    pub isolation: Isolation,
    pub needs: Option<&'a str>,
}

pub struct Outcome {
    pub status: i32,
    pub seconds: u64,
}

pub trait Backend {
    fn run(&self, req: &Request, command: &str) -> Result<Outcome, String>;
    /// Same, but the job's stdout comes back rather than going to the terminal. For setup
    /// steps that have to report where they put things; a benchmark's output must keep
    /// streaming to whoever asked for it.
    fn run_capture(&self, req: &Request, command: &str) -> Result<(Outcome, String), String>;
    fn name(&self) -> &'static str;
}

/// The bash wrapper. Exclusive maps to `--bench`, shared to a plain call.
///
/// `needs` and per-device isolation have nowhere to go here: this backend knows one machine
/// and does not know what is in it. Rather than pretend, it refuses, because silently running
/// a tensor-core benchmark on whatever card happens to be free is the failure that routing
/// exists to prevent.
pub struct Dibs {
    pub program: String,
    /// Chosen once per run and held for every step. Picking per step would put the build on one
    /// machine and the command that needs its worktree on another.
    pub machine: Option<String>,
}

impl Default for Dibs {
    fn default() -> Self {
        Dibs { program: "dibs".into(), machine: None }
    }
}

impl Dibs {
    /// Asks the wrapper to rank the inventory. Only for recipes that are shared throughout: a
    /// measurement's history keys on the machine it ran on, so moving one silently merges two
    /// distributions under a single label.
    ///
    /// `prefer` names the machine already holding this repo's build cache. Without it a build
    /// can land on one machine and the benchmark that needs what it built on another, which
    /// leaves the benchmark to compile inside its own exclusive lock.
    /// The machine this would go to with no ranking at all. Naming it matters even when there
    /// was no choice to make: a benchmark cannot be moved, so it is the one that decides where
    /// its repo's build cache belongs, and the record should say where it ran.
    pub fn which(program: &str) -> Option<String> {
        let out = Command::new(program).arg("--which").stdin(Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!name.is_empty()).then_some(name)
    }

    pub fn routed(program: &str, prefer: Option<&str>, repo: Option<&str>) -> Option<String> {
        let mut cmd = Command::new(program);
        cmd.arg("--pick");
        if let Some(p) = prefer {
            cmd.arg("--prefer").arg(p);
        }
        // The recorded preference is a memo; asking which machines actually hold the cache is
        // what makes the first run for a repo land somewhere useful.
        if let Some(r) = repo {
            cmd.arg("--repo").arg(r);
        }
        let out = cmd.stdin(Stdio::null()).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!name.is_empty()).then_some(name)
    }
}

impl Backend for Dibs {
    fn run(&self, req: &Request, command: &str) -> Result<Outcome, String> {
        let mut cmd = self.build(req, command)?;
        let start = std::time::Instant::now();
        let status = cmd
            .status()
            .map_err(|e| format!("could not run {}: {e}", self.program))?;
        Ok(Outcome {
            status: status.code().unwrap_or(-1),
            seconds: start.elapsed().as_secs(),
        })
    }

    fn run_capture(&self, req: &Request, command: &str) -> Result<(Outcome, String), String> {
        let mut cmd = self.build(req, command)?;
        cmd.stderr(Stdio::inherit());   // or a failing fetch says only "exit 3"
        let start = std::time::Instant::now();
        let out = cmd
            .output()
            .map_err(|e| format!("could not run {}: {e}", self.program))?;
        Ok((
            Outcome {
                status: out.status.code().unwrap_or(-1),
                seconds: start.elapsed().as_secs(),
            },
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ))
    }

    fn name(&self) -> &'static str {
        "dibs"
    }
}

impl Dibs {
    fn build(&self, req: &Request, command: &str) -> Result<Command, String> {
        if req.isolation == Isolation::Device {
            return Err("per-device isolation needs a backend that knows what is in the machine; \
                        this one locks the whole machine or nothing"
                .into());
        }
        if let Some(n) = req.needs {
            return Err(format!(
                "this recipe needs '{n}', and the dibs backend cannot check that or route on it"
            ));
        }
        let mut cmd = Command::new(&self.program);
        if let Some(m) = &self.machine {
            cmd.arg("--on").arg(m);
        }
        if req.lock == Lock::Exclusive {
            cmd.arg("--bench");
        }
        cmd.arg("--label").arg(req.label);
        // Tells the wrapper this came through the interface, so it does not print the note
        // that points at the interface.
        cmd.env("DIBS_FROM_RUN", "1");
        cmd.arg(command);
        // A job must never inherit this process's stdin: the wrapper reads its own channel to
        // learn that the caller is gone, and a shared stdin makes that signal meaningless.
        cmd.stdin(Stdio::null());
        Ok(cmd)
    }
}
