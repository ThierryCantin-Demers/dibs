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
    jobs::{JobRequest, Jobs},
    local::Repo,
};
use crate::{
    call::RecipeJob,
    gitdeps::{CargoHome, Db, GitPin},
    recipe::Lock,
};
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
    pub gitdbs: Vec<Db>,
}

impl TreeSpec<'_> {
    pub fn plan(&self) -> TreePlan {
        let lock = Repo(self.dir).lockfile(self.local.is_none().then_some(self.reference));
        let lock = lock.as_deref().unwrap_or("");
        let gitdbs = CargoHome::here().dbs(&GitPin::all(lock));
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

    /// Sends each git database the machine said it lacks.
    pub fn send_missing(&self, backend: &Jobs, prepared: &wire::Prepared) {
        let Some(asked) = &prepared.gitdbs else {
            return;
        };
        for missing in &asked.missing {
            let Some(db) = self
                .gitdbs
                .iter()
                .find(|d| d.name == missing.name && d.commit == missing.commit)
            else {
                continue;
            };
            eprintln!(
                "dibs: sending {} at {:.8}, which the machine does not have and may not be able to fetch",
                db.name, db.commit
            );
            if !sync_gitdb(backend, &db.path, &format!("{}/{}", asked.dir, db.name)) {
                eprintln!(
                    "dibs: sending {} failed; the build will try to fetch it itself",
                    db.path.display()
                );
            }
        }
    }
}

/// What a job that only lays out a tree runs, which `--status` and `--log` show of it.
pub fn preparing_title(repo: &str, reference: &str) -> String {
    format!("# prepare {repo}@{reference}")
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

/// What a prepare left, said to the person reading along.
pub trait Announce {
    fn announce(&self);
}

impl Announce for wire::Prepared {
    fn announce(&self) {
        eprintln!("dibs: {}", self.worktree);
        let Some(seeded) = &self.seeded else {
            return;
        };
        let from = &seeded.from;
        if let (Some(mine), Some(shared)) = (self.reseeded, seeded.shared) {
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
                eprintln!(
                    "dibs: target directory copied from {from}, so only what differs rebuilds"
                )
            }
        }
    }
}

/// Adds files and never replaces one: git names objects by their content, so what is already
/// there is already right, and a cargo on the machine may be reading it. Says whether it was sent.
fn sync_gitdb(backend: &Jobs, from: &Path, to: &str) -> bool {
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
    backend.sync(&req, &args, &mut |_| {}).outcome.status == 0
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
