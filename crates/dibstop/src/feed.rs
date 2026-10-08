//! One watch per machine, read in this process by the dibs library and reported as messages.

use dibs::{
    call::{Fed, StatusFeed},
    inventory::Inventory,
};
use dibs_format::{MachineName, status::Status};
use std::sync::mpsc::Sender;

/// `generation` tells a feed replaced by a change of interval from the one that replaced it, so
/// the old one's closing words are not read as the new one dying.
pub enum Msg {
    State {
        generation: u64,
        machine: String,
        status: Box<Status>,
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

/// A running feed. Dropping it stops it, and its watch on the machine ends with the connection.
pub struct Feed {
    pub machine: String,
    _watch: StatusFeed,
}

impl Feed {
    pub fn spawn(tx: Sender<Msg>, machine: String, interval: u64, generation: u64) -> Feed {
        let named = (!machine.is_empty()).then(|| MachineName::new(&machine));
        let from = machine.clone();
        let watch = StatusFeed::start(named, interval, move |fed| {
            let machine = from.clone();
            let msgs = match fed {
                Fed::Status(status) => vec![Msg::State {
                    generation,
                    machine,
                    status,
                }],
                Fed::Unreadable(text) => vec![Msg::Trouble {
                    generation,
                    machine,
                    text,
                }],
                Fed::Ended { said, why } => {
                    let trouble = (!said.is_empty()).then(|| Msg::Trouble {
                        generation,
                        machine: machine.clone(),
                        text: said,
                    });
                    trouble
                        .into_iter()
                        .chain([Msg::Ended {
                            generation,
                            machine,
                            why,
                        }])
                        .collect()
                }
            };
            for msg in msgs {
                let _ = tx.send(msg);
            }
        });
        Feed {
            machine,
            _watch: watch,
        }
    }
}

/// The machines to watch. Empty means there is no inventory, or one that does not read, and the
/// one feed goes wherever a bare `dibs` would, which says what is wrong with it.
pub fn machines() -> Vec<String> {
    Inventory::pool()
        .map(|names| names.into_iter().collect())
        .unwrap_or_default()
}
