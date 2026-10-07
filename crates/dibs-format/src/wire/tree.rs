use crate::Pairs;
use serde::{Deserialize, Serialize};

/// The tree a job runs in: prepared at its head under its lock, or one prepared before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    pub place: Place,
    pub then: Then,
    /// What the runner does around a recipe step's command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<Step>,
}

/// What surrounds a recipe step's command in its tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// A shared build claims the target for its tree, dating the tree's sources after the
    /// artifacts another tree left there.
    pub claim: bool,
    /// A build: the prepare whose staged lockfile joins the target's record once it exits 0.
    pub record: Option<String>,
    /// A measurement after a build: refused when another tree has built into the target since,
    /// since the binary there may be that tree's.
    pub check: bool,
    /// A measurement, which records the state the machine was in.
    pub state: bool,
    /// Files the step keeps beside its log: paths in the tree, or under `$CARGO_TARGET_DIR/`.
    pub artifacts: Vec<String>,
    /// Crates a pin replaces, which must no longer come from where they came before.
    pub pinned: Vec<String>,
}

/// What the runner did around a step's command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stepped {
    /// The measurement was refused before it ran.
    pub refused: bool,
    /// The machine's state as the measurement started, values it had none for left out.
    pub state: Option<Pairs>,
    /// How many files the step kept, when it kept any.
    pub artifacts: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    Prepare(Prepare),
    At(At),
}

/// A tree already prepared, and the build cache it is given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct At {
    pub worktree: String,
    pub target: String,
}

/// What the machine needs to lay a tree out under its scratch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepare {
    /// The clone's directory name, under `~/prog` on the machine.
    pub repo: String,
    pub source: Source,
    pub nest: Option<Nest>,
    /// Paths a new tree starts without when its sources are copied from a sibling's.
    pub fresh: Vec<String>,
    pub packages: Option<Packages>,
    /// Git dependencies this computer could send, which the machine is asked whether it lacks.
    pub gitdbs: Vec<GitDb>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A ref the machine fetches; `slot` is which of a comparison's fetched arms it is.
    Fetched { reference: String, slot: u32 },
    /// A tree this computer sends; `content` names what it holds.
    Local { key: String, content: String },
}

/// The directory a pinned tree is nested in, and the cargo config that points it at the pins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nest {
    pub name: String,
    pub config: String,
}

/// A lockfile as the short sorted hashes a seed and a target's record compare, and the prepare
/// they are staged under until a build of them succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Packages {
    pub token: String,
    pub lines: Vec<String>,
}

/// A pinned git commit, and the directory in cargo's git cache that holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDb {
    pub name: String,
    pub commit: String,
}

/// What follows a prepare in the same job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Then {
    /// The job is the prepare.
    Nothing,
    /// A recipe step, run in the worktree with the target exported; it waits while a git
    /// dependency is missing.
    Step,
    /// rsync's far side, in the worktree's parent, since the transfer names the tree's directory.
    Transfer,
}

/// What a prepare left, which the client says and records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepared {
    pub worktree: String,
    pub target: String,
    pub revision: Revision,
    pub seeded: Option<Seeded>,
    /// How many of its lockfile's groups this tree's own target had, when a sibling's replaced it.
    pub reseeded: Option<u64>,
    pub gitdbs: Option<GitDbs>,
}

impl Prepared {
    /// This tree as laid out, for a step that runs in it.
    pub fn tree(&self) -> Tree {
        Tree {
            place: Place::At(At {
                worktree: self.worktree.clone(),
                target: self.target.clone(),
            }),
            then: Then::Step,
            step: None,
        }
    }

    /// Whether the tree waits for a git dependency to be sent before it can build.
    pub fn awaits_gitdbs(&self) -> bool {
        self.gitdbs.as_ref().is_some_and(|g| !g.missing.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub repo: String,
    pub sha: String,
}

/// The sibling target a new one was copied from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seeded {
    pub from: String,
    /// How many of this tree's packages the sibling had built, out of how many.
    pub shared: Option<Shared>,
    /// The sibling's sources came too, so unchanged files keep the times they were built at.
    pub sources: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shared {
    pub have: u64,
    pub of: u64,
}

/// Where the machine's cargo keeps git databases, and which of those asked about it lacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDbs {
    pub dir: String,
    pub missing: Vec<GitDb>,
}
