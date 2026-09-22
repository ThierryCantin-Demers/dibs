//! The document `dibs --watch --json` prints once per interval.

use serde::Deserialize;

use crate::text::dur;

#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    pub state: String,
    #[serde(default)]
    pub holders: Vec<Holder>,
    #[serde(default)]
    pub queue: Vec<Queued>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Holder {
    pub mode: String,
    pub pid: i64,
    pub label: String,
    pub agent: String,
    #[serde(default)]
    pub device: Option<String>,
    pub cmd: String,
    pub elapsed: i64,
    pub cpu: i64,
    #[serde(default)]
    pub cpu_rate: Option<i64>,
    pub est: Option<i64>,
    pub est_n: Option<i64>,
    pub est_scope: Option<String>,
    pub remaining: Option<i64>,
    pub overrun: Option<bool>,
    pub idle_for: Option<i64>,
    pub idle_kind: Option<String>,
    /// The file this job redirected into, when it redirected at all. Absent means `o` has
    /// nothing to show, and saying so beats offering a key that does nothing.
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub batch: Option<Batch>,
}

/// The batch a job is a step of, as the machine sees it: the steps still to come there and the
/// time the batch has left there, queue included. Absent for a job that is not in a batch.
#[derive(Debug, Clone, Deserialize)]
pub struct Batch {
    pub id: String,
    pub step: String,
    pub k: i64,
    pub n: i64,
    #[serde(default)]
    pub next: String,
    #[serde(default)]
    pub far: String,
    pub left: Option<i64>,
    #[serde(default)]
    pub left_partial: bool,
}

impl Batch {
    pub fn left_text(&self) -> Option<String> {
        self.left.map(|l| match self.left_partial {
            true => format!("over {}", dur(l)),
            false => format!("~{}", dur(l)),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Queued {
    pub position: i64,
    pub mode: String,
    pub pid: i64,
    pub label: String,
    pub agent: String,
    #[serde(default)]
    pub device: Option<String>,
    pub cmd: String,
    pub waiting: i64,
    #[serde(default)]
    pub eta: Option<i64>,
    #[serde(default)]
    pub batch: Option<Batch>,
}
