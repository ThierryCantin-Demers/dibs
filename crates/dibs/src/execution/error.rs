use crate::{
    batch::BatchError,
    call::CallError,
    fleet::FleetError,
    git::GitError,
    inventory::InventoryError,
    paths::FileError,
    recipe::{NotTaken, RecipeError, RepoError},
    records::RecordsError,
    reports::ReportsError,
};
use dibs_format::Exit;
use std::{fmt, io, path::PathBuf};

/// Why a commit could not be checked out here to be sent.
#[derive(Debug)]
pub enum CheckoutError {
    NoHome,
    File(FileError),
    /// A path git cannot take as an argument.
    Unnamed(PathBuf),
    Git(GitError),
}

/// Why the trees `@<ref>` names could not be looked up.
#[derive(Debug)]
pub enum ArmError {
    /// `A...B`, which compares nothing dibs measures.
    Symmetric(String),
    /// A range missing an end, or mixed with a list.
    HalfRange(String),
    EmptyArm(String),
    Twice {
        reference: String,
        arm: String,
    },
    NoCommit {
        name: String,
        dir: PathBuf,
    },
    NoHistory {
        from: String,
        tip: String,
        dir: PathBuf,
    },
    /// The tip of a range is its base.
    NothingToCompare {
        tip: String,
    },
    Checkout(CheckoutError),
}

/// Why a `--pin` could not be resolved.
#[derive(Debug)]
pub enum PinError {
    Malformed(String),
    /// The pin names the repo being built.
    Itself {
        pin: String,
        repo: String,
    },
    Twice {
        pin: String,
        repo: String,
    },
    NoRef {
        pin: String,
        reference: String,
        dir: PathBuf,
    },
    /// A lockfile source a `[patch]` cannot redirect.
    Unpatchable {
        name: String,
        source: String,
    },
    /// The lockfiles take none of the pinned repo's crates from anywhere a pin replaces.
    NothingToReplace {
        pinned: String,
        repo: String,
    },
    Repo(RepoError),
    Checkout(CheckoutError),
    Git(GitError),
}

impl From<FileError> for CheckoutError {
    fn from(e: FileError) -> CheckoutError {
        CheckoutError::File(e)
    }
}

impl From<GitError> for CheckoutError {
    fn from(e: GitError) -> CheckoutError {
        CheckoutError::Git(e)
    }
}

impl From<CheckoutError> for ArmError {
    fn from(e: CheckoutError) -> ArmError {
        ArmError::Checkout(e)
    }
}

impl From<GitError> for ArmError {
    fn from(e: GitError) -> ArmError {
        ArmError::Checkout(CheckoutError::Git(e))
    }
}

impl From<RepoError> for PinError {
    fn from(e: RepoError) -> PinError {
        PinError::Repo(e)
    }
}

impl From<CheckoutError> for PinError {
    fn from(e: CheckoutError) -> PinError {
        PinError::Checkout(e)
    }
}

impl From<GitError> for PinError {
    fn from(e: GitError) -> PinError {
        PinError::Git(e)
    }
}

impl fmt::Display for CheckoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckoutError::NoHome => f.write_str("no HOME to keep a cache under"),
            CheckoutError::File(e) => e.fmt(f),
            CheckoutError::Unnamed(dir) => {
                write!(f, "{}: not a path git can take", dir.display())
            }
            CheckoutError::Git(e) => e.fmt(f),
        }
    }
}

impl fmt::Display for ArmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArmError::Symmetric(r) => write!(
                f,
                "{r}: A...B is not a comparison dibs makes. A..B measures B against where it left A"
            ),
            ArmError::HalfRange(r) => write!(
                f,
                "{r}: a range names both ends, as main..local. Several arms in turn are a,b,c"
            ),
            ArmError::EmptyArm(r) => write!(f, "{r}: an empty arm"),
            ArmError::Twice { reference, arm } => write!(f, "{reference}: {arm} is named twice"),
            ArmError::NoCommit { name, dir } => write!(f, "no {name} in {}", dir.display()),
            ArmError::NoHistory { from, tip, dir } => write!(
                f,
                "{from} and {tip:.8} share no history in {}",
                dir.display()
            ),
            ArmError::NothingToCompare { tip } => write!(
                f,
                "{tip} has nothing its base does not, so there is nothing to compare"
            ),
            ArmError::Checkout(e) => e.fmt(f),
        }
    }
}

impl fmt::Display for PinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PinError::Malformed(pin) => write!(
                f,
                "--pin {pin}: a pin is one tree, <repo>@local or <repo>@<ref>"
            ),
            PinError::Itself { pin, repo } => write!(
                f,
                "--pin {pin}: that is the repo being built; name its tree with {repo}@<ref> instead"
            ),
            PinError::Twice { pin, repo } => write!(f, "--pin {pin}: {repo} is pinned twice"),
            PinError::NoRef {
                pin,
                reference,
                dir,
            } => write!(
                f,
                "--pin {pin}: no {reference} in {}, nor on its origin",
                dir.display()
            ),
            PinError::Unpatchable { name, source } => write!(
                f,
                "{name} comes from {source}, which a pin does not know how to replace"
            ),
            PinError::NothingToReplace { pinned, repo } => write!(
                f,
                "--pin {pinned}: {repo}'s Cargo.lock takes none of {pinned}'s crates from git or crates.io, so there is nothing\n  \
                 for the pin to replace. A repo already built from a path, as a local-development block does, needs no pin."
            ),
            PinError::Repo(e) => e.fmt(f),
            PinError::Checkout(e) => e.fmt(f),
            PinError::Git(e) => e.fmt(f),
        }
    }
}

/// What stops a recipe run, said as its `Display` in full, and the exit it ends with.
#[derive(Debug)]
pub enum RunError {
    /// A call the run made failed. Its exit passes on, so an unreachable machine reads as 69 to
    /// whoever ran this rather than as a refusal.
    Call {
        exit: i32,
        failed: Unprepared,
    },
    /// A call refused before it was sent, in its own words.
    Unsent(CallError),
    Recipe(RecipeError),
    Batch(BatchError),
    Records(RecordsError),
    Arm(ArmError),
    Pin(PinError),
    Git(GitError),
    Reports(ReportsError),
    Fleet(FleetError),
    Inventory(InventoryError),
    File(FileError),
    Refused(Refusal),
}

/// What a run's own words ask for that it refuses before anything is sent.
#[derive(Debug)]
pub enum Refusal {
    /// A measurement with no machine named, by its recipe's name.
    Unmeasured(String),
    /// Shared work no machine could be placed for, with the repo it was placed by.
    Nowhere(Option<String>),
    RawReason,
    RawCommand,
    RawNotTaken(Vec<NotTaken>),
    WithRefs,
    WithPin,
    /// `with` given no service, by the repo it was given.
    WithService(String),
    NoServices(String),
    NoSuchService {
        name: String,
        repo: String,
        have: Vec<String>,
    },
    /// A service with nothing to serve.
    StartsNothing(String),
    WithCommand,
    WithUnplaced,
    /// `--there` on a verb other than `with`.
    There,
    BatchStdin(io::Error),
}

/// A tree a run could not lay out on the machine.
#[derive(Debug)]
pub enum Unprepared {
    SendPinned {
        repo: String,
        from: PathBuf,
    },
    PreparePinned {
        repo: String,
        reference: String,
    },
    /// The setup ended without saying where it put the tree.
    NoPath,
    /// An arm, as `preparing` describes it.
    Prepare(String),
    PrepareFrom {
        repo: String,
        from: PathBuf,
    },
    PrepareRef {
        repo: String,
        reference: String,
    },
    Send(PathBuf),
}

impl RunError {
    pub fn exit(&self) -> u8 {
        let refused = Exit::Refused.code();
        let exit = match self {
            RunError::Call { exit, .. } => *exit,
            RunError::Unsent(e) => e.exit(),
            RunError::Recipe(_)
            | RunError::Batch(_)
            | RunError::Records(_)
            | RunError::Arm(_)
            | RunError::Pin(_)
            | RunError::Git(_)
            | RunError::Reports(_)
            | RunError::Fleet(_)
            | RunError::Inventory(_)
            | RunError::File(_)
            | RunError::Refused(_) => return refused,
        };
        u8::try_from(exit)
            .ok()
            .filter(|c| *c != 0)
            .unwrap_or(refused)
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Call { exit, failed } => {
                f.write_str("dibs: ")?;
                failed.say(*exit, f)?;
                writeln!(f)
            }
            RunError::Unsent(e) => e.fmt(f),
            RunError::Recipe(e) => writeln!(f, "dibs: {e}"),
            RunError::Batch(e) => writeln!(f, "dibs: {e}"),
            RunError::Records(e) => writeln!(f, "dibs: {e}"),
            RunError::Arm(e) => writeln!(f, "dibs: {e}"),
            RunError::Pin(e) => writeln!(f, "dibs: {e}"),
            RunError::Git(e) => writeln!(f, "dibs: {e}"),
            RunError::Reports(e) => writeln!(f, "dibs: {e}"),
            RunError::Fleet(e) => writeln!(f, "dibs: {e}"),
            RunError::Inventory(e) => writeln!(f, "dibs: {e}"),
            RunError::File(e) => writeln!(f, "dibs: {e}"),
            RunError::Refused(e) => writeln!(f, "dibs: {e}"),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Unmeasured(name) => write!(
                f,
                "{name} measures, and a measurement names its machine: its series belongs to the machine it\n  \
                 ran on. Give --on <machine>, or export DIBS_ON; dibs --machines lists them."
            ),
            Refusal::Nowhere(repo) => write!(
                f,
                "nowhere to send this: it names no machine, and none could be placed. dibs --pick -v{}\n  \
                 says why; --on <machine> names one.",
                repo.as_ref().map(|r| format!(" --repo {r}")).unwrap_or_default()
            ),
            Refusal::RawReason => f.write_str(
                "raw needs --reason. It is recorded, and a reason that keeps recurring is what\n             specifies the next recipe. If this fits a recipe, use the recipe instead.",
            ),
            Refusal::RawCommand => f.write_str("raw needs -- <command>"),
            Refusal::RawNotTaken(not_taken) => {
                f.write_str("raw ")?;
                for (i, n) in not_taken.iter().enumerate() {
                    if i > 0 {
                        f.write_str(";\n  and ")?;
                    }
                    n.fmt(f)?;
                }
                f.write_str(".")
            }
            Refusal::WithRefs => f.write_str("with runs against one tree, so it takes one ref"),
            Refusal::WithPin => f.write_str("with does not take --pin; a recipe does"),
            Refusal::WithService(repo) => write!(
                f,
                "with needs a service: dibs with {repo} <service> -- <command>"
            ),
            Refusal::NoServices(repo) => write!(
                f,
                "{repo} defines no services, so there is nothing to run against"
            ),
            Refusal::NoSuchService { name, repo, have } => write!(
                f,
                "no service '{name}' for {repo}. It has: {}",
                have.join(", ")
            ),
            Refusal::StartsNothing(name) => write!(
                f,
                "service '{name}' starts nothing: it needs a [[service.{name}.serve]] with a run"
            ),
            Refusal::WithCommand => f.write_str("with needs a command after --"),
            Refusal::WithUnplaced => f.write_str(
                "with starts servers on a machine this computer then drives, so it names one: --on <machine>,\n  \
                 or export DIBS_ON. dibs --machines lists them.",
            ),
            Refusal::There => f.write_str(
                "--there belongs to with: it runs the command on the machine beside the repo's servers",
            ),
            Refusal::BatchStdin(e) => write!(f, "reading the batch from stdin: {e}"),
        }
    }
}

impl Unprepared {
    fn say(&self, exit: i32, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unprepared::SendPinned { repo, from } => write!(
                f,
                "could not send the pinned {repo} from {} {}",
                from.display(),
                Ended(exit)
            ),
            Unprepared::PreparePinned { repo, reference } => write!(
                f,
                "could not prepare the pinned {repo}@{reference} {}",
                Ended(exit)
            ),
            Unprepared::NoPath => {
                f.write_str("the worktree setup did not report a path; see its output above")
            }
            Unprepared::Prepare(what) => write!(f, "could not prepare {what} {}", Ended(exit)),
            Unprepared::PrepareFrom { repo, from } => write!(
                f,
                "could not prepare {repo} from {} {}",
                from.display(),
                Ended(exit)
            ),
            Unprepared::PrepareRef { repo, reference } => {
                write!(f, "could not prepare {repo}@{reference} {}", Ended(exit))
            }
            Unprepared::Send(dir) => write!(f, "sending {} failed (exit {exit})", dir.display()),
        }
    }
}

/// A failed prepare's exit. A 0 means the call ended well and never said what tree it laid out.
struct Ended(i32);

impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            0 => f.write_str("(exit 0, yet no tree was reported back)"),
            exit => write!(f, "(exit {exit})"),
        }
    }
}

impl From<CallError> for RunError {
    fn from(e: CallError) -> RunError {
        RunError::Unsent(e)
    }
}

impl From<RecipeError> for RunError {
    fn from(e: RecipeError) -> RunError {
        RunError::Recipe(e)
    }
}

impl From<RepoError> for RunError {
    fn from(e: RepoError) -> RunError {
        RunError::Recipe(RecipeError::Repo(e))
    }
}

impl From<BatchError> for RunError {
    fn from(e: BatchError) -> RunError {
        RunError::Batch(e)
    }
}

impl From<RecordsError> for RunError {
    fn from(e: RecordsError) -> RunError {
        RunError::Records(e)
    }
}

impl From<ArmError> for RunError {
    fn from(e: ArmError) -> RunError {
        RunError::Arm(e)
    }
}

impl From<PinError> for RunError {
    fn from(e: PinError) -> RunError {
        RunError::Pin(e)
    }
}

impl From<GitError> for RunError {
    fn from(e: GitError) -> RunError {
        RunError::Git(e)
    }
}

impl From<ReportsError> for RunError {
    fn from(e: ReportsError) -> RunError {
        RunError::Reports(e)
    }
}

impl From<FleetError> for RunError {
    fn from(e: FleetError) -> RunError {
        RunError::Fleet(e)
    }
}

impl From<InventoryError> for RunError {
    fn from(e: InventoryError) -> RunError {
        RunError::Inventory(e)
    }
}

impl From<FileError> for RunError {
    fn from(e: FileError) -> RunError {
        RunError::File(e)
    }
}

impl From<Refusal> for RunError {
    fn from(e: Refusal) -> RunError {
        RunError::Refused(e)
    }
}
