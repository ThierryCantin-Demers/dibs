use crate::{Alias, Label, Mode, wire::Tree};
use serde::{Deserialize, Serialize};

/// One call, as the runner is asked to make it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub mode: Mode,
    /// As the machine files it.
    pub label: Label,
    /// One shell string, run by `bash -c`.
    pub command: String,
    pub wait: Option<u64>,
    /// Seconds the job may hold the lock; 0 is no cap.
    pub max: u64,
    pub max_from: MaxFrom,
    pub verbose: bool,
    pub json: bool,
    /// The whole output rather than its digest.
    pub stream: bool,
    /// The caller's stdout is a terminal.
    pub tty: bool,
    pub card: Option<Card>,
    /// The cards after `card` a job over several is pinned to, in the order named.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<Card>,
    pub fingerprint: Option<String>,
    /// The caller's session title.
    pub agent: String,
    /// The caller's session id, which ownership is decided by.
    pub agent_id: String,
    /// The Claude subagent within that session that made the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    /// The batch step this call is, as `batch.<pid>` holds it: the id and step, `k` of `n`, then
    /// the steps still to come.
    pub batch: Option<String>,
    pub watch: Watch,
    /// `--port` names, each given a free port the job reads as `$DIBS_PORT_<NAME>`.
    pub ports: Vec<String>,
    /// `--with` servers, run for the job's length.
    pub services: Vec<Service>,
    /// Seconds the services have to be ready in.
    pub ready_within: u64,
    /// The measurement starts its label's series on this machine again, on its card.
    pub new_series: bool,
    /// Only compiles, so it may start ahead of a queued benchmark and be frozen while one runs.
    #[serde(default)]
    pub preempt: bool,
    /// Waits for a machine closed by its owners to open, rather than being refused.
    #[serde(default)]
    pub when_open: bool,
    /// A status that reads the machine's CPU, memory and cards as well.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub metrics: bool,
    /// Each card's `--device` alias by its PCI address, from the caller's inventory, which is
    /// what a metrics reading names a card by.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub aliases: std::collections::BTreeMap<String, String>,
    /// The tree the job runs in, prepared at its head when it is not yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<Tree>,
}

/// A machine taken back by its owners.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Close {
    /// Stops the jobs running rather than letting them finish.
    pub now: bool,
    /// Accounts whose work carries on.
    pub except: Vec<String>,
    pub reason: Option<String>,
}

impl Close {
    /// One `key=value` a line, as a `--close` call sends it and the lock directory's `closed`
    /// holds it.
    pub fn text(&self) -> String {
        let mut text = format!("now={}\n", u8::from(self.now));
        if !self.except.is_empty() {
            text.push_str(&format!("except={}\n", self.except.join(",")));
        }
        if let Some(reason) = &self.reason {
            text.push_str(&format!("reason={}\n", reason.replace('\n', " ")));
        }
        text
    }

    /// What `text` wrote; anything else is passed over, so an empty text closes to everyone.
    pub fn of_text(text: &str) -> Close {
        let mut close = Close::default();
        for line in text.lines() {
            match line.split_once('=') {
                Some(("now", value)) => close.now = value.trim() == "1",
                Some(("except", value)) => {
                    close.except = value
                        .split(',')
                        .map(str::trim)
                        .filter(|a| !a.is_empty())
                        .map(String::from)
                        .collect();
                }
                Some(("reason", value)) => {
                    close.reason = Some(value.trim().to_string()).filter(|r| !r.is_empty());
                }
                _ => {}
            }
        }
        close
    }
}

/// A server run on the machine for one call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub command: String,
    /// `tcp:[host:]port`, the port possibly a `--port` name, or a command that exits 0 once the
    /// server can be used. None is ready at once.
    pub ready: Option<String>,
}

/// Where `--max` came from, which decides whether history may raise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaxFrom {
    Given,
    Default,
}

impl Request {
    /// The cards a call is pinned to as `--device` named them, which its series is filed under.
    pub fn device(&self) -> Option<Alias> {
        let first = self.card.as_ref()?;
        Some(Alias::new(
            std::iter::once(first)
                .chain(&self.also)
                .map(|c| c.alias.as_str())
                .collect::<Vec<_>>()
                .join(","),
        ))
    }
}

/// The card a call is pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub alias: Alias,
    /// The slot it was in when the machine was probed; None for a device with no slot.
    pub pci: Option<String>,
    pub runtimes: Vec<String>,
    /// `vendor:device`, as the machine's probe read it.
    pub chip: Option<String>,
    /// How many cards there share its chip id.
    pub twins: usize,
}

/// How the runner learns its caller is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    /// Nothing to watch: the stream after the request is not the caller's.
    pub off: bool,
    /// The lock is held for a command run elsewhere, which ends with a `release`.
    pub hold: bool,
    /// Seconds of silence that count as gone; 0 waits for the stream to end.
    pub lease: u64,
}
