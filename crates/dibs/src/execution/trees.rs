//! Getting to the point where cargo can be run at all.
//!
//! In the log this replaces, 107 of 179 jobs named a hand-written worktree path and only 9
//! began with `cargo`: nearly all the length of a typical command was fetching a ref, adding a
//! worktree, and arranging a build cache, with six agents each having invented their own
//! version including whether to `mv` or `cp -a` a sibling's target directory.
//!
//! So dibs owns the layout and nothing is asked to follow a convention by hand. The machine
//! lays a tree out under the shared lock, because it is a fetch and a checkout: work that
//! tolerates neighbours perfectly and must never hold the exclusive lock.

use super::{
    build::hex,
    jobs::{JobRequest, Jobs, Reported},
};
use crate::{call::RecipeJob, gitdeps, recipe::Lock};
use dibs_format::wire;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Where a pinned tree lives, and the `[patch]` that points its build at the pinned trees. The
/// config sits in the directory above the tree, where cargo reads it after the tree's own, so the
/// tree stays exactly what was sent or checked out. The name is a hash of the config, so every
/// tree built against one set of pins shares it and no two sets write the same file.
pub struct Nest {
    pub name: String,
    pub config: String,
}

impl Nest {
    pub fn new(config: String) -> Nest {
        Nest {
            name: format!("pin-{:.10}", hex(&Sha256::digest(config.as_bytes()))),
            config,
        }
    }
}

/// How a local tree is sent. `--checksum` without `--times` is what the seed relies on: a file
/// whose bytes match is left alone with the time it was copied with, and any other is rewritten
/// and takes the current time.
/// The marker is excluded so `--delete` leaves it, or collection could never date the tree.
pub const SYNC_ARGS: &[&str] = &[
    "-rlpgo",
    "--checksum",
    "--no-times",
    "--delete",
    "--exclude=.git",
    "--exclude=/.dibs-used",
    "--filter=:- .gitignore",
];

/// What the machine needs to know to prepare one tree.
pub struct TreeSpec<'a> {
    pub dir: &'a Path,
    pub repo_name: &'a str,
    pub reference: &'a str,
    pub local: Option<&'a super::Local>,
    pub signature: &'a str,
    pub token: &'a str,
    pub slot: usize,
    pub nest: Option<&'a Nest>,
    pub fresh: &'a [String],
}

/// A tree as the machine is asked to lay it out, and the git databases it may need sent.
pub struct TreePlan {
    pub prepare: wire::Prepare,
    pub gitdbs: Vec<gitdeps::Db>,
}

impl TreeSpec<'_> {
    pub fn plan(&self) -> TreePlan {
        let lock = lockfile(self.dir, self.local.is_none().then_some(self.reference));
        let lock = lock.as_deref().unwrap_or("");
        let gitdbs = gitdeps::local(&gitdeps::cargo_home(), &gitdeps::pinned(lock));
        let lines = super::packages(lock, self.signature);
        let prepare = wire::Prepare {
            repo: self.repo_name.to_string(),
            source: match self.local {
                Some(l) => wire::Source::Local {
                    key: l.key.clone(),
                    content: l.content.clone(),
                },
                None => wire::Source::Fetched {
                    reference: self.reference.to_string(),
                    slot: self.slot as u32,
                },
            },
            nest: self.nest.map(|n| wire::Nest {
                name: n.name.clone(),
                config: n.config.clone(),
            }),
            fresh: self.fresh.to_vec(),
            packages: (!lines.is_empty()).then(|| wire::Packages {
                token: self.token.to_string(),
                lines,
            }),
            gitdbs: gitdbs
                .iter()
                .map(|db| wire::GitDb {
                    name: db.name.clone(),
                    commit: db.commit.clone(),
                })
                .collect(),
        };
        TreePlan { prepare, gitdbs }
    }
}

impl TreePlan {
    /// Laid out at the head of a job, which then does `then`.
    pub fn tree(&self, then: wire::Then) -> wire::Tree {
        wire::Tree {
            place: wire::Place::Prepare(self.prepare.clone()),
            then,
            step: None,
        }
    }
}

/// A step in a tree laid out before.
pub fn in_tree(prepared: &wire::Prepared) -> wire::Tree {
    wire::Tree {
        place: wire::Place::At(wire::At {
            worktree: prepared.worktree.clone(),
            target: prepared.target.clone(),
        }),
        then: wire::Then::Step,
        step: None,
    }
}

/// What a job that only lays out a tree runs, which `--status` and `--log` show of it.
pub fn preparing_title(repo: &str, reference: &str) -> String {
    format!("# prepare {repo}@{reference}")
}

/// The lockfile of the tree here, or of a ref in its history.
pub fn lockfile(dir: &Path, reference: Option<&str>) -> Option<String> {
    match reference {
        None => std::fs::read_to_string(dir.join("Cargo.lock")).ok(),
        Some(r) => std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["show", &format!("{r}:Cargo.lock")])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()),
    }
}

/// Unique per invocation, and what a build's package list is staged under until it succeeds.
pub fn new_token() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

pub fn send_missing_gitdbs(backend: &Jobs, prepared: &wire::Prepared, gitdbs: &[gitdeps::Db]) {
    let Some(asked) = &prepared.gitdbs else {
        return;
    };
    for missing in &asked.missing {
        let Some(db) = gitdbs
            .iter()
            .find(|d| d.name == missing.name && d.commit == missing.commit)
        else {
            continue;
        };
        eprintln!(
            "dibs: sending {} at {:.8}, which the machine does not have and may not be able to fetch",
            db.name, db.commit
        );
        if let Err(e) = sync_gitdb(backend, &db.path, &format!("{}/{}", asked.dir, db.name)) {
            eprintln!("dibs: {e}; the build will try to fetch it itself");
        }
    }
}

/// Whether a step's tree waits for a git dependency to be sent before it can build.
pub fn held(prepared: &wire::Prepared) -> bool {
    prepared
        .gitdbs
        .as_ref()
        .is_some_and(|g| !g.missing.is_empty())
}

/// Prepares the worktree and sends the local tree into it, as one job under one lock.
///
/// `--no-times` is the load-bearing option and it is not tidiness. rsync's `-a` implies `-t`,
/// which is right for a transfer and wrong for sources about to be compiled: files that arrive
/// carrying an older mtime than the artifacts already beside them leave cargo with nothing to
/// do, so the build finishes in a fraction of a second and the previous binary is what gets
/// measured. It reads exactly like a fast incremental build. `--checksum` is what makes
/// dropping `-t` affordable, because without it every destination mtime differs on the next
/// pass and the whole tree goes again each time.
///
/// The filter follows the repo's own ignore rules, so a target directory or an editor's
/// droppings never make the trip, and `--delete` means a file deleted locally stops existing
/// there too rather than going on compiling.
pub fn sync_prepared(
    backend: &Jobs,
    from: &Path,
    key: &str,
    req: &JobRequest,
    on_prepared: &mut dyn FnMut(&wire::Prepared),
) -> Reported {
    let args: Vec<String> = SYNC_ARGS
        .iter()
        .map(|a| a.to_string())
        .chain([format!("{}/", from.display()), format!(":local-{key}/")])
        .collect();
    backend.sync(req, &args, on_prepared)
}

pub fn announce_prepared(prepared: &wire::Prepared) {
    eprintln!("dibs: {}", prepared.worktree);
    let Some(seeded) = &prepared.seeded else {
        return;
    };
    let from = &seeded.from;
    if let (Some(mine), Some(shared)) = (prepared.reseeded, seeded.shared) {
        eprintln!(
            "dibs: this tree's target had built {mine} of the {} groups in its lockfile and {from} has {}, so the tree now starts from {from}'s",
            shared.of, shared.have
        );
        return;
    }
    match seeded.shared {
        Some(shared) => eprintln!(
            "dibs: target directory copied from {from}, whose builds match {} of the {} groups in this tree's lockfile{}",
            shared.have,
            shared.of,
            if seeded.sources {
                ", with its sources so unchanged crates stay built"
            } else {
                ""
            }
        ),
        None => {
            eprintln!("dibs: target directory copied from {from}, so only what differs rebuilds")
        }
    }
}

/// Adds files and never replaces one: git names objects by their content, so what is already
/// there is already right, and a cargo on the machine may be reading it.
pub fn sync_gitdb(backend: &Jobs, from: &Path, to: &str) -> Result<(), String> {
    let args = gitdb_args(from, to);
    let req = JobRequest {
        label: "",
        lock: Lock::Shared,
        device: None,
        job: &RecipeJob::default(),
        max: None,
        new_series: false,
        tree: None,
    };
    match backend.sync(&req, &args, &mut |_| {}).outcome.status {
        0 => Ok(()),
        _ => Err(format!("sending {} failed", from.display())),
    }
}

pub fn gitdb_args(from: &Path, to: &str) -> [String; 5] {
    [
        "-a".to_string(),
        "--no-times".to_string(),
        "--ignore-existing".to_string(),
        format!("{}/", from.display()),
        format!(":{to}/"),
    ]
}
