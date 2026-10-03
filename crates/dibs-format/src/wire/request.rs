use crate::{Alias, Label, Mode};
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
    pub fingerprint: Option<String>,
    /// The caller's session title.
    pub agent: String,
    /// The caller's session id, which ownership is decided by.
    pub agent_id: String,
    /// The batch step this call is, as `batch.<pid>` holds it: the id and step, `k` of `n`, then
    /// the steps still to come.
    pub batch: Option<String>,
    pub watch: Watch,
}

/// Where `--max` came from, which decides whether history may raise it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaxFrom {
    Given,
    Default,
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
