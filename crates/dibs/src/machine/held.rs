use crate::machine::session::{Message, Started, exit_code};
use dibs_format::wire::Picked;
use std::{
    io::{self, BufRead as _, BufReader},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
};

/// The line a bash machine half prints once the lock is held, with each picked port after it.
const HOLDING: &str = "DIBS-HOLDING";

/// The machine's side of a hold, started: what it says is passed on as it comes.
pub struct Holder {
    /// Says once, when the lock is held; closes when the machine's side ends.
    pub held: Receiver<Held>,
    /// The machine side's exit.
    pub ended: JoinHandle<io::Result<i32>>,
}

/// The lock is held for the command here.
pub struct Held {
    pub ports: Vec<Picked>,
    pub release: Release,
}

/// Ends a hold with its command's status. Dropped unsent, the machine learns its caller is gone.
pub struct Release(pub(super) Sender<Message>);

impl Release {
    pub fn send(self, status: i32) {
        let _ = self.0.send(Message::Release(status));
    }
}

impl Holder {
    /// A bash machine half's hold: its stdout, a line at a time, until it says the lock is held.
    pub fn of_payload(started: Started) -> Holder {
        let Started { mut child, channel } = started;
        let stdout = child.stdout.take().expect("a hold's stdout is piped");
        let (tell, held) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut release = Some(channel);
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                match line.strip_prefix(HOLDING) {
                    Some(ports) if ports.is_empty() || ports.starts_with(' ') => {
                        if let Some(channel) = release.take() {
                            let _ = tell.send(Held {
                                ports: picked(ports),
                                release: Release(channel),
                            });
                        }
                    }
                    _ => println!("{line}"),
                }
            }
        });
        let ended = thread::spawn(move || {
            let status = child.wait();
            let _ = reader.join();
            status.map(exit_code)
        });
        Holder { held, ended }
    }
}

/// ` name=port` pairs, as a bash machine half prints them.
fn picked(ports: &str) -> Vec<Picked> {
    ports
        .split_whitespace()
        .filter_map(|p| {
            let (name, port) = p.split_once('=')?;
            Some(Picked {
                name: name.to_string(),
                port: port.parse().ok()?,
            })
        })
        .collect()
}
