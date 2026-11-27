use crate::Pairs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The tree a job runs in: prepared at its head under its lock, or one prepared before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    pub place: Place,
    pub then: FollowUp,
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
    /// Files the step keeps beside its log: paths in the tree, or under `$CARGO_TARGET_DIR/`,
    /// kept in the order named.
    pub artifacts: Vec<String>,
    /// Crates a pin replaces, which must no longer come from where they came before, each looked
    /// for in the tree's lockfiles.
    pub pinned: Vec<String>,
}

/// What the runner did around a step's command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepReport {
    /// The measurement was refused before it ran.
    pub refused: bool,
    /// The machine's state as the measurement started, values it had none for left out.
    pub state: Option<Pairs>,
    /// How many files the step kept, when it kept any.
    #[serde(rename = "artifacts")]
    pub artifact_count: Option<u32>,
}

/// Where a job's tree comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    /// Laid out at the job's head.
    Prepare(Box<Prepare>),
    /// Laid out by an earlier call.
    At(PreparedTree),
}

/// A tree already prepared, and the build cache it is given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedTree {
    pub worktree: String,
    pub target: String,
    /// Each variable a step gets, and the target's copy of a machine directory it names, all
    /// set on the step's command.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub copies: BTreeMap<String, String>,
}

/// What the machine needs to lay a tree out under its scratch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepare {
    /// The clone's directory name, under `~/prog` on the machine.
    pub repo: String,
    pub source: Source,
    pub nest: Option<Nest>,
    /// Paths a new tree starts without when its sources are copied from a sibling's, each
    /// removed in turn.
    pub fresh: Vec<String>,
    /// Directories of the machine's, by the variable that names each, that the tree's target
    /// keeps a copy of for what runs there: a tool's store that one binary rewrites for itself.
    /// Each is copied in turn.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub copies: BTreeMap<String, String>,
    /// The file at the top of a copied directory, looked up by its variable, naming the version
    /// that wrote it: such a directory holds one version's files, which only that version reads.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stamps: BTreeMap<String, String>,
    /// The version the tree's root Cargo.toml gives, which is what a stamp is compared with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub packages: Option<Packages>,
    /// Git dependencies this computer could send, each of which the machine is asked whether it
    /// lacks, in turn.
    pub gitdbs: Vec<GitDb>,
}

/// Where a prepared tree's sources come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A ref the machine fetches; `slot` is which of a comparison's fetched arms it is, and
    /// `here` the commit it was where the call was made, taken when origin no longer has it.
    Fetched {
        reference: String,
        slot: u32,
        #[serde(default)]
        here: Option<String>,
    },
    /// A tree this computer sends; `content` names what it holds, and `target` the build cache
    /// it shares with the other commits sent from one checkout, when it is one of those.
    Local {
        key: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
}

/// The directory a pinned tree is nested in, the cargo config that points it at the pins, and
/// the pinned trees to put beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nest {
    pub name: String,
    pub config: String,
    /// The pinned trees, by repo name: each laid out in turn, and asked whether it names a repo.
    #[serde(default)]
    pub siblings: BTreeMap<String, String>,
}

/// A lockfile as the short sorted hashes a seed and a target's record compare, and the prepare
/// they are staged under until a build of them succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Packages {
    pub token: String,
    /// The hashes, sorted, each written as one line of the target's record.
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
pub enum FollowUp {
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
pub struct PrepareReport {
    pub worktree: String,
    pub target: String,
    pub revision: Revision,
    pub seeded: Option<SeedReport>,
    /// How many of its lockfile's groups this tree's own target had, when a sibling's replaced it.
    #[serde(rename = "reseeded")]
    pub reseeded_count: Option<u64>,
    pub gitdbs: Option<GitDbs>,
    /// Where the target keeps its copy of each directory the prepare named, by variable, all set
    /// on a step's command.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub copies: BTreeMap<String, String>,
}

impl PrepareReport {
    /// This tree as laid out, for `step` to run in.
    pub fn tree(&self, step: Step) -> Tree {
        Tree {
            place: Place::At(PreparedTree {
                worktree: self.worktree.clone(),
                target: self.target.clone(),
                copies: self.copies.clone(),
            }),
            then: FollowUp::Step,
            step: Some(step),
        }
    }

    /// Whether the tree waits for a git dependency to be sent before it can build.
    pub fn awaits_gitdbs(&self) -> bool {
        self.gitdbs.as_ref().is_some_and(|g| !g.missing.is_empty())
    }
}

/// The commit a repo's tree was laid out at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub repo: String,
    pub sha: String,
}

/// The sibling target a new one was copied from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedReport {
    pub from: String,
    pub shared: Option<Overlap>,
    /// The sibling's sources came too, so unchanged files keep the times they were built at.
    pub sources: bool,
}

/// How many of a tree's packages a sibling had built, out of how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlap {
    #[serde(rename = "have")]
    pub have_count: u64,
    #[serde(rename = "of")]
    pub of_count: u64,
}

/// Where the machine's cargo keeps git databases, and which of those asked about it lacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDbs {
    pub dir: String,
    /// Those the client sends, in turn.
    pub missing: Vec<GitDb>,
}
