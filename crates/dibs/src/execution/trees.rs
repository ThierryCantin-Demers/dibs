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

use crate::execution::build::hex;
use sha2::{Digest, Sha256};

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
