use crate::{Alias, BatchId, JobId, Label, Mode};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One machine's lock as `dibs status` reports it: who holds it, who waits and when each should
/// start, and what a dispatcher ranks the machine by. Its JSON, one line, is what dibstop and
/// placement read; the fields only the text shows are left out of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// When it was read, in seconds since the epoch.
    pub t: u64,
    pub state: LockState,
    pub cores: u64,
    /// The one-minute load average, times 100.
    pub load: u64,
    /// The repos it has a build cache for.
    pub caches: Vec<String>,
    /// The repos it has a clone of, which a worktree can be prepared from.
    pub clones: Vec<String>,
    pub holders: Vec<Holder>,
    /// In arrival order.
    pub queue: Vec<Waiter>,
    #[serde(skip)]
    pub scene: Scene,
}

impl Status {
    /// The document as one line of JSON, which a reader takes a line at a time.
    pub fn line(&self) -> String {
        format!("{}\n", serde_json::to_string(self).unwrap_or_default())
    }
}

/// Who holds the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockState {
    Idle,
    Shared,
    Bench,
    /// Taken with no holder record yet, as by a client between taking it and recording it.
    Busy,
    /// Taken by a process no record accounts for, which outlived its session.
    Orphan,
}

/// What only the text shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scene {
    /// The machine's short name, which `dibs --on` reaches it by.
    pub host: String,
    pub orphans: Vec<Orphan>,
    /// The lock is taken, and no process this account can see holds it.
    pub unseen: bool,
    /// The lock directory, for a caller that asked with `-v`.
    pub listing: Option<Listing>,
}

/// A process holding the lock that no record names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    pub pid: u32,
    /// Its pid, age, owner and arguments, as `ps -o pid=,etime=,user=,args=` prints them.
    pub described: String,
}

/// The lock directory's entries, and how many runs the history holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub dir: PathBuf,
    pub entries: Vec<String>,
    pub history: PathBuf,
    pub runs: usize,
}

/// A job holding the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub mode: Mode,
    pub pid: u32,
    /// None for a job a bash machine half started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<JobId>,
    pub label: Label,
    pub agent: String,
    #[serde(default, with = "dashed")]
    pub device: Option<Alias>,
    pub cmd: String,
    pub started: u64,
    pub elapsed: u64,
    /// CPU seconds its whole tree has used, reaped children included.
    pub cpu: u64,
    #[serde(flatten)]
    pub estimate: Option<Shown>,
    /// The first file its tree writes its output to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Cores' worth of CPU, in hundredths, since the last look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_rate: Option<u64>,
    #[serde(flatten)]
    pub idle: Option<Idle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchShown>,
    #[serde(skip)]
    pub services: Vec<Service>,
}

/// A job waiting for the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiter {
    /// Counting from 1, in arrival order.
    pub position: usize,
    pub mode: Mode,
    pub pid: u32,
    pub label: Label,
    pub agent: String,
    #[serde(default, with = "dashed")]
    pub device: Option<Alias>,
    pub cmd: String,
    pub arrived: u64,
    pub waiting: u64,
    /// Seconds until it should start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchShown>,
}

/// What a holder's history says it takes, and where that leaves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shown {
    #[serde(rename = "est")]
    pub median: u64,
    /// The tenth percentile.
    #[serde(rename = "est_lo")]
    pub low: u64,
    /// The ninetieth percentile.
    #[serde(rename = "est_hi")]
    pub high: u64,
    #[serde(rename = "est_n")]
    pub runs: usize,
    #[serde(rename = "est_scope")]
    pub scope: Scope,
    /// The runs spread too far for the median to predict anything.
    #[serde(rename = "est_wide", default, skip_serializing_if = "is_false")]
    pub wide: bool,
    /// Drawn from the label's other procedures, since this one has not run.
    #[serde(rename = "est_other_values", default, skip_serializing_if = "is_false")]
    pub other: bool,
    /// None once it has run longer than its history says it ever takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_kind: Option<Remaining>,
    /// Running more than twice its ninetieth percentile.
    #[serde(default, skip_serializing_if = "is_false")]
    pub overrun: bool,
}

/// Which key an estimate was drawn from, sharpest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// This label, and this procedure where the job named one.
    This,
    /// What the agent's jobs in this mode usually take.
    Agent,
    /// What any job in this mode takes.
    Mode,
}

/// What the time left is measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Remaining {
    /// The median, not yet reached.
    Typical,
    /// The ninetieth percentile, once past the median.
    Bound,
}

/// A holder whose tree has used no CPU for a while.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Idle {
    pub idle_for: u64,
    pub idle_kind: IdleKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdleKind {
    /// No CPU at all since it started.
    Never,
    /// None since the last look that saw it working.
    Stalled,
}

/// A `--with` server run beside a holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub pid: u32,
    pub command: String,
}

/// The batch a job is a step of, and what the batch still has to do on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchShown {
    pub id: BatchId,
    pub step: String,
    pub k: usize,
    pub n: usize,
    /// Steps still to come here, then elsewhere.
    pub here: usize,
    pub elsewhere: usize,
    /// Those steps by name, each with its estimate here.
    pub next: String,
    pub far: String,
    #[serde(flatten)]
    pub left: Option<Left>,
}

/// How long the batch has left on this machine, queue included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Left {
    pub left: u64,
    /// Some of what is ahead has no history, so this is a floor.
    pub left_partial: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A card as the records write it: `-` for none.
mod dashed {
    use crate::Alias;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<Alias>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(value.as_ref().map_or("-", Alias::as_str))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Alias>, D::Error> {
        let text = String::deserialize(d)?;
        Ok(match text.as_str() {
            "-" | "" => None,
            _ => Some(Alias::new(text)),
        })
    }
}
