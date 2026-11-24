//! What a `dibs` client and the orchestrator say to each other over ssh: one `Ask` line, then
//! `Reply` lines until `End`. Unlike the runner's frames this is versioned, since a coworker's
//! dibs and the orchestrator's are built from different sources.

use crate::wire::{Admitted, Trailer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The version of this conversation a client speaks.
pub const VERSION: u32 = 1;

/// The command a client asks the orchestrator's account to run; its forced command reads it from
/// `SSH_ORIGINAL_COMMAND`.
pub const API_COMMAND: &str = "dibs-orchestrator api";

/// Where an upload lands, under the coworker's own staging directory.
pub const STAGE_PREFIX: &str = "stage/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub v: u32,
    pub call: Call,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Call {
    Submit(Box<Submission>),
    /// The asker's submissions, or everyone's.
    List {
        all: bool,
    },
    Status {
        id: String,
    },
    /// What a step said: the worker's record of its calls, or with `job`, the job's whole log
    /// from its machine.
    Out {
        id: String,
        step: Option<String>,
        job: bool,
    },
    /// Waits for the submission to end, or for `check_in` seconds.
    Follow {
        id: String,
        check_in: u64,
    },
    Kill {
        id: String,
    },
    Inventory,
}

/// Work handed to the orchestrator, acknowledged once its plan and inputs are stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    /// The client's, so the same submission sent twice is stored once.
    pub key: String,
    /// The agent or person under the coworker's account, for attribution.
    pub agent: String,
    pub work: Work,
    /// The tree sent ahead of the submission, for `@local`.
    pub upload: Option<Upload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Work {
    /// A call on one named machine.
    Call(NamedCall),
    /// A recipe built on the compile pool and run on the GPU pool.
    Split(SplitCall),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedCall {
    pub on: String,
    pub bench: bool,
    pub device: Option<String>,
    pub label: String,
    pub command: String,
    pub max: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitCall {
    pub repo: String,
    pub recipe: String,
    pub values: BTreeMap<String, String>,
}

/// A working tree sent by rsync into `stage/<stage>` before the submission, sealed by it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upload {
    pub stage: String,
    pub repo: String,
    /// The commit its HEAD names.
    pub head: String,
    /// `Seal::digest` of the files sent beside `.git`.
    pub digest: String,
    pub files: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reply {
    Accepted {
        id: String,
        /// The key was seen before: this is the submission it made then.
        duplicate: bool,
    },
    Submissions(Vec<Summary>),
    Status(Detail),
    Inventory {
        machines: String,
        pools: BTreeMap<String, Vec<String>>,
    },
    /// Text for the caller's stdout, or with `err`, its stderr.
    Text {
        err: bool,
        text: String,
    },
    /// A step's job ended, or its attempt was found admitted before.
    Trailer {
        step: String,
        trailer: Trailer,
    },
    Admitted {
        step: String,
        admitted: Admitted,
    },
    End {
        exit: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Stored, its worker not yet started.
    Accepted,
    Running,
    Done,
    Failed,
    Cancelled,
    /// A measurement stopped partway, which is never run again on its own.
    Interrupted,
    /// How it went cannot be known, and it is never run again on its own.
    Unknown,
}

impl State {
    pub fn ended(self) -> bool {
        !matches!(self, State::Accepted | State::Running)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            State::Accepted => "accepted",
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
            State::Interrupted => "interrupted",
            State::Unknown => "unknown",
        }
    }
}

impl std::str::FromStr for State {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [
            State::Accepted,
            State::Running,
            State::Done,
            State::Failed,
            State::Cancelled,
            State::Interrupted,
            State::Unknown,
        ]
        .into_iter()
        .find(|state| state.as_str() == s)
        .ok_or_else(|| format!("{s:?} is no submission state"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub id: String,
    pub coworker: String,
    pub agent: String,
    /// The work in one line, as `dibs orch list` shows it.
    pub what: String,
    pub state: State,
    /// Seconds since the epoch.
    pub created: u64,
    pub exit: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detail {
    pub summary: Summary,
    pub steps: Vec<StepState>,
    /// What the worker said outside its steps, the reason a submission failed before any.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepState {
    pub name: String,
    pub machine: Option<String>,
    pub state: State,
    pub attempts: u32,
    pub trailer: Option<Trailer>,
    pub note: Option<String>,
}

/// The digest an upload is sealed by: each file's path and SHA-256, in path order, a symlink by
/// the path it holds. The client computes it over what it sends, the orchestrator over what
/// arrived, and a submission whose two differ is never accepted.
pub struct Seal;

impl Seal {
    pub fn of<'a>(
        root: &std::path::Path,
        files: impl IntoIterator<Item = &'a str>,
    ) -> std::io::Result<(String, u64)> {
        let mut files: Vec<&str> = files.into_iter().collect();
        files.sort_unstable();
        files.dedup();
        let mut sealed = crate::Sha256::default();
        for file in &files {
            let path = root.join(file);
            let digest = match std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                true => {
                    crate::Sha256::hex(std::fs::read_link(&path)?.as_os_str().as_encoded_bytes())
                }
                false => crate::Sha256::hex_of_file(&path)?,
            };
            sealed.update(file.as_bytes());
            sealed.update(b"\0");
            sealed.update(digest.as_bytes());
            sealed.update(b"\n");
        }
        Ok((crate::Hex::hex(&sealed.finish()[..]), files.len() as u64))
    }

    /// Every file under `root` but its `.git`, as paths relative to it.
    pub fn listed(root: &std::path::Path) -> std::io::Result<Vec<String>> {
        let mut found = Vec::new();
        let mut dirs = vec![std::path::PathBuf::new()];
        while let Some(rel) = dirs.pop() {
            for entry in std::fs::read_dir(root.join(&rel))? {
                let entry = entry?;
                let path = rel.join(entry.file_name());
                if rel.as_os_str().is_empty() && entry.file_name() == ".git" {
                    continue;
                }
                if entry.file_type()?.is_dir() {
                    dirs.push(path);
                } else {
                    found.push(path.to_string_lossy().into_owned());
                }
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seal_reads_the_same_whatever_order_the_files_are_named_in() {
        let root = std::env::temp_dir().join(format!("dibs-seal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/.git")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a"), "one").unwrap();
        std::fs::write(root.join("src/b"), "two").unwrap();
        std::fs::write(root.join(".git/HEAD"), "not sealed").unwrap();
        std::fs::write(root.join("src/.git/x"), "a nested one is").unwrap();
        let mut listed = Seal::listed(&root).unwrap();
        listed.sort();
        assert_eq!(listed, ["a", "src/.git/x", "src/b"]);
        let one = Seal::of(&root, ["a", "src/b", "src/.git/x"]).unwrap();
        let two = Seal::of(&root, listed.iter().map(String::as_str)).unwrap();
        assert_eq!(one, two);
        assert_eq!(one.1, 3);
        std::fs::write(root.join("src/b"), "changed").unwrap();
        assert_ne!(Seal::of(&root, ["a", "src/b", "src/.git/x"]).unwrap(), one);
        let _ = std::fs::remove_dir_all(&root);
    }
}
