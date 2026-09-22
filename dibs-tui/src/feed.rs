//! One `dibs --watch --json` per machine, read on threads of its own and reported as messages.

use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    sync::mpsc::Sender,
    thread,
};

use crate::status::Status;

/// `generation` tells a feed replaced by a change of interval from the one that replaced it, so
/// the old one's closing words are not read as the new one dying.
pub enum Msg {
    State {
        generation: u64,
        machine: String,
        status: Status,
    },
    /// What a feed said on stderr, or a document it printed that could not be read.
    Trouble {
        generation: u64,
        machine: String,
        text: String,
    },
    Ended {
        generation: u64,
        machine: String,
        why: String,
    },
    /// What a one-off `dibs` call printed.
    Action { title: String, body: String },
}

/// A running feed. Dropping it stops it.
pub struct Feed {
    pub machine: String,
    child: Child,
}

impl Feed {
    pub fn spawn(
        tx: Sender<Msg>,
        machine: String,
        interval: u64,
        generation: u64,
    ) -> std::io::Result<Feed> {
        // Under setpriv so the feed dies with this process however this process dies. Killing the
        // feeds on the way out only covers the ways out that run code; a SIGKILL, or a terminal
        // closing on the whole thing, leaves each feed reparented to init and polling a shared
        // machine every couple of seconds until somebody notices. Four of those ran for four hours
        // before anyone did. `dibs` already relies on the same tool to make its ssh client die with
        // it, so this is the same guarantee one level up.
        let mut args: Vec<String> = Vec::new();
        if !machine.is_empty() {
            args.extend(["--on".to_string(), machine.clone()]);
        }
        args.extend([
            "--watch".to_string(),
            interval.to_string(),
            "--json".to_string(),
        ]);

        let spawn = |wrapped: bool| {
            let mut cmd = match wrapped {
                true => {
                    let mut c = Command::new("setpriv");
                    c.arg("--pdeathsig=TERM").arg("dibs");
                    c
                }
                false => Command::new("dibs"),
            };
            cmd.args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        };
        // Without setpriv the feed still works; it just outlives a hard kill, which is the state
        // this was in before and is better than not running at all.
        let mut child = match spawn(true) {
            Ok(c) => c,
            Err(_) => spawn(false)?,
        };
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        read_documents(stdout, tx.clone(), machine.clone(), generation);
        read_trouble(stderr, tx, machine.clone(), generation);
        Ok(Feed { machine, child })
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_documents(stdout: ChildStdout, tx: Sender<Msg>, machine: String, generation: u64) {
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !line.starts_with('{') {
                continue;
            }
            let msg = match serde_json::from_str::<Status>(&line) {
                Ok(status) => Msg::State {
                    generation,
                    machine: machine.clone(),
                    status,
                },
                Err(e) => Msg::Trouble {
                    generation,
                    machine: machine.clone(),
                    text: format!("unreadable document: {e}"),
                },
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
        let _ = tx.send(Msg::Ended {
            generation,
            machine,
            why: "the feed closed".into(),
        });
    });
}

fn read_trouble(stderr: ChildStderr, tx: Sender<Msg>, machine: String, generation: u64) {
    thread::spawn(move || {
        let mut s = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut s);
        let text = s.trim();
        if !text.is_empty() {
            let _ = tx.send(Msg::Trouble {
                generation,
                machine,
                text: text.to_string(),
            });
        }
    });
}

/// The machines to watch. Empty means there is no inventory, and the one feed goes wherever a
/// bare `dibs` would: a single machine looks exactly as it did before any of this existed.
pub fn machines() -> Vec<String> {
    match Command::new("dibs").arg("--machines").output() {
        Ok(out) if out.status.success() => parse_machines(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

/// `--machines` marks the default with a leading `*`, which is part of the line rather than
/// part of the name.
fn parse_machines(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            match it.next() {
                Some("*") => it.next(),
                other => other,
            }
            .map(str::to_string)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_marker_is_not_part_of_the_name() {
        let listing = " * bench1 dibs@bench1\n   laptop     laptop  (no measurements)\n";
        assert_eq!(parse_machines(listing), vec!["bench1", "laptop"]);
    }

    #[test]
    fn no_inventory_is_no_machines_rather_than_one_empty_name() {
        assert!(parse_machines("").is_empty());
    }
}
