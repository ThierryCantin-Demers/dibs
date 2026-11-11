//! The one-off `dibs` calls a person starts from a view, whose output it shows over itself.

use dibs_format::Label;
use serde::{Deserialize, Serialize};

use crate::item::Item;

const TREE: &str = r#"ps -eo pid=,ppid=,stat=,etime=,time=,args= | awk -v r=PIDHERE '{p[NR]=$1;q[NR]=$2;l[NR]=$0} END {w[r]=1; do {c=0; for(i=1;i<=NR;i++) if(w[q[i]]&&!w[p[i]]){w[p[i]]=1;c=1}} while(c); for(i=1;i<=NR;i++) if(w[p[i]]) print substr(l[i],1,200)}'"#;

/// Each vendor's tool where its driver is loaded: a machine with no NVIDIA card has no
/// nvidia-smi, and rocm-smi on one with no AMD card prints a driver error.
const GPUS: &str = "s=; if [ -d /sys/module/nvidia ] && command -v nvidia-smi >/dev/null; then nvidia-smi; s=1; fi; if [ -d /sys/module/amdgpu ] && command -v rocm-smi >/dev/null; then rocm-smi; s=1; fi; [ -n \"$s\" ] || echo 'neither nvidia-smi with an NVIDIA driver nor rocm-smi with an AMD one is here'";

const LOG_LINES: usize = 40;

/// Which call an action is, enough to make the same one again in another process, as the
/// one that runs a web page's actions does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Call {
    Log,
    Gpu,
    Status,
    Output { pid: u32 },
    Tree { pid: u32 },
    Kill { pid: u32 },
}

pub struct Action {
    pub machine: String,
    pub call: Call,
    /// The label of the job a call on one is about.
    pub label: String,
    /// What the header says while it runs.
    pub doing: String,
    pub title: String,
    pub args: Vec<String>,
}

impl Action {
    pub fn new(machine: String, call: Call, label: &str) -> Action {
        let (doing, title, args): (String, String, Vec<String>) = match call {
            Call::Log => (
                "reading the log".into(),
                "recent arrivals and outcomes".into(),
                vec!["--log".into(), LOG_LINES.to_string()],
            ),
            Call::Gpu => (
                "asking the GPUs".into(),
                "GPUs".into(),
                vec!["--peek".into(), GPUS.into()],
            ),
            Call::Status => (
                "refreshing".into(),
                "status".into(),
                vec!["--status".into()],
            ),
            Call::Output { pid } => (
                format!("reading what {pid} is writing"),
                format!("output of {pid} ({label})"),
                vec!["--out".into(), pid.to_string()],
            ),
            Call::Tree { pid } => (
                format!("looking at {pid}"),
                format!("process tree under {pid} ({label})"),
                vec!["--peek".into(), TREE.replace("PIDHERE", &pid.to_string())],
            ),
            Call::Kill { pid } => (
                format!("stopping {label}"),
                format!("kill {pid} ({label})"),
                // The person's y is the say-so that --anyone asks for.
                vec!["--kill".into(), pid.to_string(), "--anyone".into()],
            ),
        };
        Action {
            machine,
            call,
            label: label.to_string(),
            doing,
            title,
            args,
        }
    }

    pub fn log(machine: String) -> Action {
        Action::new(machine, Call::Log, "")
    }

    pub fn gpu(machine: String) -> Action {
        Action::new(machine, Call::Gpu, "")
    }

    pub fn status(machine: String) -> Action {
        Action::new(machine, Call::Status, "")
    }

    pub fn output(job: &Item) -> Action {
        Action::new(
            job.machine.clone(),
            Call::Output { pid: job.pid },
            job.label.as_str(),
        )
    }

    pub fn process_tree(job: &Item) -> Action {
        Action::new(
            job.machine.clone(),
            Call::Tree { pid: job.pid },
            job.label.as_str(),
        )
    }

    pub fn kill(machine: String, pid: u32, label: &Label) -> Action {
        Action::new(machine, Call::Kill { pid }, label.as_str())
    }

    /// Runs it, and says what it printed, both streams, fit to show.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run(&self) -> String {
        let body = match std::process::Command::new("dibs")
            .args(self.command_line())
            .output()
        {
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                let e = String::from_utf8_lossy(&o.stderr);
                if !e.trim().is_empty() {
                    if !s.is_empty() {
                        s.push('\n');
                    }
                    s.push_str(&e);
                }
                if s.trim().is_empty() {
                    s = format!("(nothing, exit {})", o.status.code().unwrap_or(-1));
                }
                s
            }
            Err(e) => format!("could not run dibs: {e}"),
        };
        readable(&body)
    }

    /// The whole `dibs` command line after the program's name.
    pub fn command_line(&self) -> Vec<String> {
        let on = match self.machine.is_empty() {
            true => Vec::new(),
            false => vec!["--on".to_string(), self.machine.clone()],
        };
        on.into_iter().chain(self.args.iter().cloned()).collect()
    }
}

/// A job's output is whatever the job printed, and build tools print two things this cannot
/// render: colour escapes, and the carriage returns a progress line redraws itself with.
/// Passed through, the first corrupts the terminal rather than the paragraph, and the second
/// makes one line look like several overlaid.
pub fn readable(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\u{1b}' => match it.peek() {
                // CSI, terminated by a byte in @ through ~. Colour is all of these here.
                Some('[') => {
                    it.next();
                    for c2 in it.by_ref() {
                        if ('@'..='~').contains(&c2) {
                            break;
                        }
                    }
                }
                // OSC, terminated by BEL or ST. Terminal titles, mostly.
                Some(']') => {
                    it.next();
                    for c2 in it.by_ref() {
                        if c2 == '\u{7}' || c2 == '\u{1b}' {
                            break;
                        }
                    }
                }
                _ => {
                    it.next();
                }
            },
            '\r' => {}
            '\t' => out.push_str("    "),
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_and_progress_redraws_do_not_reach_the_screen() {
        let cargo = "\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m cubecl v0.1\n\
                     \r    Blocking waiting for file\rok\n";
        assert_eq!(
            readable(cargo),
            "   Compiling cubecl v0.1\n    Blocking waiting for fileok\n"
        );
    }

    #[test]
    fn newlines_survive_because_the_paragraph_needs_them() {
        assert_eq!(readable("a\nb\tc\u{0}d"), "a\nb    cd");
    }
}
