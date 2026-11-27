//! What a `dibs` client and the dibspatcher say to each other over ssh: one `Ask` line, then
//! `Reply` lines until `End`. Unlike the runner's frames this is versioned, since a coworker's
//! dibs and the dibspatcher's are built from different sources.

use crate::{
    Hex, Sha256,
    wire::{Admission, Trailer},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io, path::Path};

/// The version of this conversation a client speaks. A dibspatcher refuses an `Ask` of any
/// other, telling its client to update dibs.
///
/// # Guarantees
///
/// - It goes up whenever a dibspatcher could misread what a client of the version before sends
///   it: a call or a reply read another way, or a [`Seal`] computed another way.
pub const VERSION: u32 = 1;

/// The command a client asks the dibspatcher's account to run; its forced command reads it from
/// `SSH_ORIGINAL_COMMAND`.
pub const API_COMMAND: &str = "dibspatcher api";

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

/// Work handed to the dibspatcher, acknowledged once its plan and inputs are stored.
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
    /// The recipe's values by name, read by name and in name order.
    pub values: BTreeMap<String, String>,
}

/// A working tree sent by rsync into `stage/<stage>` before the submission, sealed by it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upload {
    pub stage: String,
    pub repo: String,
    /// The commit its HEAD names.
    pub head: String,
    /// What the client sent beside `.git`.
    #[serde(flatten)]
    pub seal: Seal,
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
        /// Each inventory file by its path under the config directory, `machines.toml` and
        /// `machines.d/<name>.toml`, for a client that keeps the team's machines.
        /// None from a dibspatcher that keeps no copy for its clients; empty for a team with no
        /// machines.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        files: Option<BTreeMap<String, String>>,
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
        admitted: Admission,
    },
    End {
        exit: i32,
    },
}

/// Where a submission or one of its steps stands.
///
/// # Guarantees
///
/// - `from_str` reads back every name `as_str` gives, and refuses any other.
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

impl State {
    const ALL: [State; 7] = [
        State::Accepted,
        State::Running,
        State::Done,
        State::Failed,
        State::Cancelled,
        State::Interrupted,
        State::Unknown,
    ];
}

impl std::str::FromStr for State {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        State::ALL
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
    /// The work in one line, as `dibs dispatch list` shows it.
    pub what: String,
    pub state: State,
    /// When it was stored, in seconds since the epoch.
    #[serde(rename = "created")]
    pub created_secs: u64,
    pub exit: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detail {
    pub summary: Summary,
    /// In plan order, and read in it, or for the step a caller names.
    pub steps: Vec<StepState>,
    /// What the worker said outside its steps, the reason a submission failed before any.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepState {
    pub name: String,
    pub machine: Option<String>,
    pub state: State,
    #[serde(rename = "attempts")]
    pub attempt_count: u32,
    pub trailer: Option<Trailer>,
    pub note: Option<String>,
}

/// The seal of an upload: a digest of each file's path and contents, in path order, and how many
/// files it covers. The client computes it over what it sends, the dibspatcher over what
/// arrived, and a submission whose two differ is never accepted.
///
/// # Guarantees
///
/// - The digest is the same whatever order the files are named in, and however often one is.
/// - A symlink is sealed by the path it holds.
/// - `file_count` counts each file named once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seal {
    pub digest: String,
    #[serde(rename = "files")]
    pub file_count: u64,
}

impl Seal {
    /// The seal of the files named, as paths relative to `root`.
    ///
    /// # Assumptions
    ///
    /// - Each name is a UTF-8, relative, `/`-separated path, as both the client and the
    ///   dibspatcher list them: the digest seals the name as it is written.
    ///
    /// # Errors
    ///
    /// The error reading a file named, or the path a link holds.
    pub fn of<'a>(root: &Path, files: impl IntoIterator<Item = &'a str>) -> io::Result<Seal> {
        let mut files: Vec<&str> = files.into_iter().collect();
        files.sort_unstable();
        files.dedup();
        let mut sealed = Sha256::default();
        for file in &files {
            sealed.update(sealed_line(&root.join(file), file)?.as_bytes());
        }
        Ok(Seal {
            digest: Hex::new(&sealed.finish()).to_string(),
            file_count: files.len() as u64,
        })
    }
}

/// One file's line of a seal: its path and the digest of what it holds, which for a link is the
/// path it holds.
fn sealed_line(path: &Path, file: &str) -> io::Result<String> {
    let digest = match fs::symlink_metadata(path)?.file_type().is_symlink() {
        true => Sha256::hex(fs::read_link(path)?.as_os_str().as_encoded_bytes()),
        false => Sha256::hex_of_file(path)?,
    };
    Ok(format!("{file}\0{digest}\n"))
}

#[cfg(test)]
mod seal_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr as _;

    #[test]
    fn every_state_reads_back_from_its_name() {
        for state in State::ALL {
            assert_eq!(State::from_str(state.as_str()), Ok(state));
        }
    }

    #[test]
    fn a_name_no_state_has_is_refused() {
        assert_eq!(
            State::from_str("Done"),
            Err(r#""Done" is no submission state"#.to_string())
        );
    }

    #[test]
    fn an_ask_reads_its_call_by_its_tag() {
        let asks = [
            r#"{"v":1,"call":"inventory"}"#,
            r#"{"v":1,"call":{"list":{"all":true}}}"#,
            r#"{"v":1,"call":{"follow":{"id":"s","check_in":30}}}"#,
        ]
        .map(|line| serde_json::from_str::<Ask>(line).unwrap().call);
        assert_eq!(
            asks,
            [
                Call::Inventory,
                Call::List { all: true },
                Call::Follow {
                    id: "s".into(),
                    check_in: 30
                },
            ]
        );
    }

    #[test]
    fn a_reply_reads_by_its_tag() {
        let replies = [
            r#"{"accepted":{"id":"s","duplicate":false}}"#,
            r#"{"text":{"err":true,"text":"x"}}"#,
            r#"{"end":{"exit":3}}"#,
        ]
        .map(|line| serde_json::from_str::<Reply>(line).unwrap());
        assert_eq!(
            replies,
            [
                Reply::Accepted {
                    id: "s".into(),
                    duplicate: false
                },
                Reply::Text {
                    err: true,
                    text: "x".into()
                },
                Reply::End { exit: 3 },
            ]
        );
    }

    #[test]
    fn an_inventory_from_a_dibspatcher_keeping_no_files_reads_with_none() {
        let reply: Reply =
            serde_json::from_str(r#"{"inventory":{"machines":"m","pools":{}}}"#).unwrap();
        assert_eq!(
            reply,
            Reply::Inventory {
                machines: "m".into(),
                pools: BTreeMap::new(),
                files: None
            }
        );
    }

    #[test]
    fn an_upload_writes_its_seal_beside_its_head() {
        let upload = Upload {
            stage: "k".into(),
            repo: "r".into(),
            head: "h".into(),
            seal: Seal {
                digest: "d".into(),
                file_count: 2,
            },
        };
        assert_eq!(
            serde_json::to_string(&upload).unwrap(),
            r#"{"stage":"k","repo":"r","head":"h","digest":"d","files":2}"#
        );
    }
}
