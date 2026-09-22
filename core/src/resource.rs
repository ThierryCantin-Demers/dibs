//! Getting a machine with the right things held, which is the only thing the layer below is
//! for.
//!
//! Today that is `dibs`, a bash script that ships itself over ssh and takes an flock. It could
//! become `srun` against a Slurm cluster without anything above this file changing, which is
//! the entire reason the boundary is here: the crossover is the multi-device machine, and it
//! should be a swap rather than a rewrite.

use crate::recipe::{Isolation, Lock};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Instant;

pub struct Request<'a> {
    pub label: &'a str,
    pub lock: Lock,
    pub isolation: Isolation,
    pub needs: Option<&'a str>,
    /// The card to run on, named from the machine's inventory. Absent means the runtime
    /// picks, which is fine for a build and is what makes two benchmarks incomparable.
    pub device: Option<&'a str>,
    /// Which batch this is a step of and what is still to come, for the machine's status.
    pub env: &'a [(&'static str, String)],
    /// Seconds the job may hold the lock, when the caller knows the default is too short.
    pub max: Option<u64>,
    /// A measurement that starts its label's series on this machine again, on another card.
    pub new_series: bool,
}

pub struct Outcome {
    pub status: i32,
    pub seconds: u64,
    /// What the job's trailer said, when its stderr passed through here and it printed one.
    pub trailer: Option<Trailer>,
}

pub struct Trailer {
    pub job: String,
    /// `built=` as printed: a count of crates, or `nothing`. Absent when cargo did not run.
    pub built: Option<String>,
    /// `<host>:<path>` of the job's whole log.
    pub log: Option<String>,
}

impl Trailer {
    /// Reads `job <id>  <mode>  <label>  ...  built=N` and the `  log <host>:<path>` after it. The
    /// last job wins: a call is one job, and anything before it was a setup riding ahead.
    fn read(line: &str, into: &mut Option<Trailer>) {
        if let Some(rest) = line.strip_prefix("job ") {
            let mut words = rest.split_whitespace();
            if let Some(job) = words.next() {
                let built = words.find_map(|w| w.strip_prefix("built=")).map(str::to_string);
                *into = Some(Trailer { job: job.to_string(), built, log: None });
            }
        } else if let (Some(rest), Some(t)) = (line.strip_prefix("  log "), into.as_mut()) {
            t.log = rest.split_whitespace().next().map(str::to_string);
        }
    }
}

pub trait Backend {
    fn run(&self, req: &Request, command: &str) -> Result<Outcome, String>;
    /// Same, but the job's stdout comes back rather than going to the terminal. For setup
    /// steps that have to report where they put things; a benchmark's output must keep
    /// streaming to whoever asked for it.
    fn run_capture(&self, req: &Request, command: &str) -> Result<(Outcome, String), String>;
    /// Output streams as `run`'s does, except the report of a setup run ahead of the command,
    /// which is collected and handed to `on_report` before anything after it is shown.
    fn run_reporting(&self, req: &Request, command: &str, on_report: &mut dyn FnMut(&str)) -> Result<(Outcome, String), String>;
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

/// Where a call goes before anything is placed.
pub enum Destination {
    Named(String),
    /// Somewhere with no inventory name: a DIBS_HOST outside the inventory, or this computer.
    Unnamed,
    /// Nowhere until a machine is named, because several could take it.
    Unchosen,
}

impl Dibs {
    /// The machine this would go to with no ranking at all. Naming it matters even when there
    /// was no choice to make: a benchmark cannot be moved, so it is the one that decides where
    /// its repo's build cache belongs, and the record should say where it ran.
    pub fn which(program: &str) -> Destination {
        let Ok(out) = Command::new(program).arg("--which").stdin(Stdio::null()).output() else {
            return Destination::Unnamed;
        };
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match out.status.code() {
            Some(0) if !name.is_empty() => Destination::Named(name),
            Some(2) => Destination::Unchosen,
            _ => Destination::Unnamed,
        }
    }

    /// Asks the wrapper to rank the inventory. Only for recipes that are shared throughout: a
    /// measurement's history keys on the machine it ran on, so moving one silently merges two
    /// distributions under a single label.
    ///
    /// `prefer` names the machine already holding this repo's build cache. Without it a build
    /// can land on one machine and the benchmark that needs what it built on another, which
    /// leaves the benchmark to compile inside its own exclusive lock.
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
        Ok(reporting(self.build(req, command)?, &mut |_| {})?.0)
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
                trailer: None,
            },
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ))
    }

    fn run_reporting(&self, req: &Request, command: &str, on_report: &mut dyn FnMut(&str)) -> Result<(Outcome, String), String> {
        reporting(self.build(req, command)?, on_report)
    }

    fn name(&self) -> &'static str {
        "dibs"
    }
}

/// Runs `cmd` passing its output through, except the `DIBS-` lines of a setup's report. Both
/// streams are read: a transfer reports on stderr because rsync owns stdout, and the same
/// transfer run on the machine itself is an ordinary job whose streams arrive merged.
pub fn reporting(mut cmd: Command, on_report: &mut dyn FnMut(&str)) -> Result<(Outcome, String), String> {
    let start = Instant::now();
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {:?}: {e}", cmd.get_program()))?;
    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    let out = child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>);
    let err = child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>);
    let readers: Vec<_> = [(false, out), (true, err)]
        .into_iter()
        .filter_map(|(is_err, r)| r.map(|r| (is_err, r)))
        .map(|(is_err, r)| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(r);
                let mut line = Vec::new();
                while matches!(r.read_until(b'\n', &mut line), Ok(n) if n > 0) {
                    if tx.send((is_err, std::mem::take(&mut line))).is_err() {
                        break;
                    }
                }
            })
        })
        .collect();
    drop(tx);
    let mut report = String::new();
    let mut told = false;
    let mut trailer = None;
    for (is_err, line) in rx {
        if is_err {
            Trailer::read(String::from_utf8_lossy(&line).trim_end(), &mut trailer);
        }
        if line.starts_with(b"DIBS-") {
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end();
            report.push_str(text);
            report.push('\n');
            if !told && (text == "DIBS-READY" || text == "DIBS-HELD") {
                told = true;
                on_report(&report);
            }
            continue;
        }
        let _ = if is_err {
            let mut e = std::io::stderr().lock();
            e.write_all(&line).and_then(|_| e.flush())
        } else {
            let mut o = std::io::stdout().lock();
            o.write_all(&line).and_then(|_| o.flush())
        };
    }
    for r in readers {
        let _ = r.join();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok((Outcome { status: status.code().unwrap_or(-1), seconds: start.elapsed().as_secs(), trailer }, report))
}

impl Dibs {
    /// Whether the wrapper would refuse this request on grounds it decides here, without the
    /// machine: a machine that does not measure, or a label measured somewhere else.
    pub fn preflight(&self, req: &Request) -> Result<bool, String> {
        let mut cmd = self.flags(req)?;
        cmd.arg("--preflight").arg("true").stdin(Stdio::null());
        let status = cmd.status().map_err(|e| format!("could not run {}: {e}", self.program))?;
        Ok(status.success())
    }

    fn build(&self, req: &Request, command: &str) -> Result<Command, String> {
        let mut cmd = self.flags(req)?;
        cmd.arg(command);
        // A job must never inherit this process's stdin: the wrapper reads its own channel to
        // learn that the caller is gone, and a shared stdin makes that signal meaningless.
        cmd.stdin(Stdio::null());
        Ok(cmd)
    }

    fn flags(&self, req: &Request) -> Result<Command, String> {
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
            if req.new_series {
                cmd.arg("--new-series");
            }
        }
        cmd.arg("--label").arg(req.label);
        if let Some(m) = req.max {
            cmd.arg("--max").arg(m.to_string());
        }
        if let Some(d) = req.device {
            cmd.arg("--device").arg(d);
        }
        // Tells the wrapper this came through the interface, so it does not print the note
        // that points at the interface.
        cmd.env("DIBS_FROM_RUN", "1");
        cmd.envs(req.env.iter().map(|(k, v)| (*k, v)));
        Ok(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trailer_is_read_from_the_last_job_a_call_printed() {
        let err = "job 1-1  shared  a:setup  queued 0s  ran 1s  exit 0  by=command\n  log m:/j/1-1/log  (3 lines)  dibs --out 1-1\n\
                   job 1-2  shared  a  queued 0s  ran 9s  exit 0  by=command  built=nothing\n  log m:/j/1-2/log  (40 lines)  dibs --out 1-2\n";
        let mut t = None;
        for l in err.lines() {
            Trailer::read(l, &mut t);
        }
        let t = t.unwrap();
        assert_eq!((t.job.as_str(), t.built.as_deref(), t.log.as_deref()), ("1-2", Some("nothing"), Some("m:/j/1-2/log")));
    }

    fn request(lock: Lock, new_series: bool) -> Request<'static> {
        Request { label: "a/bench/x", lock, isolation: Isolation::Machine, needs: None, device: None, env: &[], max: None, new_series }
    }

    #[test]
    fn a_new_series_reaches_the_measurement_and_nothing_else() {
        let d = Dibs { program: "dibs".into(), machine: Some("m".into()) };
        let args = |req: &Request| d.build(req, "cmd").unwrap().get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(args(&request(Lock::Exclusive, true)), ["--on", "m", "--bench", "--new-series", "--label", "a/bench/x", "cmd"]);
        assert!(!args(&request(Lock::Shared, true)).contains(&"--new-series".to_string()));
        assert!(!args(&request(Lock::Exclusive, false)).contains(&"--new-series".to_string()));
    }
}
