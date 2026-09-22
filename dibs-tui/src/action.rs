//! One-off `dibs` calls a key starts, whose output comes back as an overlay.

use std::{process::Command, sync::mpsc::Sender, thread};

use crate::{app::PendingKill, feed::Msg, item::Item};

const TREE: &str = r#"ps -eo pid=,ppid=,stat=,etime=,time=,args= | awk -v r=PIDHERE '{p[NR]=$1;q[NR]=$2;l[NR]=$0} END {w[r]=1; do {c=0; for(i=1;i<=NR;i++) if(w[q[i]]&&!w[p[i]]){w[p[i]]=1;c=1}} while(c); for(i=1;i<=NR;i++) if(w[p[i]]) print substr(l[i],1,200)}'"#;

const LOG_LINES: usize = 40;

pub struct Action {
    machine: String,
    /// What the header says while it runs.
    pub doing: String,
    title: String,
    args: Vec<String>,
}

impl Action {
    pub fn log(machine: String) -> Action {
        Action {
            machine,
            doing: "reading the log".into(),
            title: "recent arrivals and outcomes".into(),
            args: vec!["--log".into(), LOG_LINES.to_string()],
        }
    }

    pub fn gpu(machine: String) -> Action {
        Action {
            machine,
            doing: "asking the GPU".into(),
            title: "nvidia-smi".into(),
            args: vec!["--peek".into(), "nvidia-smi".into()],
        }
    }

    pub fn status(machine: String) -> Action {
        Action {
            machine,
            doing: "refreshing".into(),
            title: "status".into(),
            args: vec!["--status".into()],
        }
    }

    pub fn output(job: &Item) -> Action {
        Action {
            machine: job.machine.clone(),
            doing: format!("reading what {} is writing", job.pid),
            title: format!("output of {} ({})", job.pid, job.label),
            args: vec!["--out".into(), job.pid.to_string()],
        }
    }

    pub fn process_tree(job: &Item) -> Action {
        Action {
            machine: job.machine.clone(),
            doing: format!("looking at {}", job.pid),
            title: format!("process tree under {} ({})", job.pid, job.label),
            args: vec![
                "--peek".into(),
                TREE.replace("PIDHERE", &job.pid.to_string()),
            ],
        }
    }

    pub fn kill(kill: PendingKill) -> Action {
        Action {
            machine: kill.machine,
            doing: format!("stopping {}", kill.label),
            title: format!("kill {} ({})", kill.pid, kill.label),
            args: vec!["--kill".into(), kill.pid.to_string()],
        }
    }

    pub fn start(self, tx: Sender<Msg>) {
        thread::spawn(move || {
            let mut cmd = Command::new("dibs");
            if !self.machine.is_empty() {
                cmd.arg("--on").arg(&self.machine);
            }
            let body = match cmd.args(&self.args).output() {
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
            let _ = tx.send(Msg::Action {
                title: self.title,
                body: plain(&body),
            });
        });
    }
}

/// A job's output is whatever the job printed, and build tools print two things this cannot
/// render: colour escapes, and the carriage returns a progress line redraws itself with.
/// Passed through, the first corrupts the terminal rather than the paragraph, and the second
/// makes one line look like several overlaid.
fn plain(s: &str) -> String {
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
            plain(cargo),
            "   Compiling cubecl v0.1\n    Blocking waiting for fileok\n"
        );
    }

    #[test]
    fn newlines_survive_because_the_paragraph_needs_them() {
        assert_eq!(plain("a\nb\tc\u{0}d"), "a\nb    cd");
    }
}
