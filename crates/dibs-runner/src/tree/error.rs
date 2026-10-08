use dibs_format::Exit;
use std::{
    fmt, io,
    path::{Component, Path, PathBuf},
};

/// What a prepare says when it fetched a ref and the machine could not see the remote at all, so
/// the ref it was asked for is very likely fine.
const CREDENTIALS: [&str; 5] = [
    "could not read Username",
    "Authentication failed",
    "terminal prompts disabled",
    "Permission denied (publickey)",
    "Repository not found",
];

/// Why a prepare laid nothing out. Its display is what the job's output says.
#[derive(Debug)]
pub enum PrepareError {
    /// A value from the wire that would name a path outside the place it is for.
    Unplaced {
        value: String,
        names: Named,
    },
    NoClone(PathBuf),
    NoRef {
        repo: String,
        reference: String,
        /// What git said, which tells a private remote from a ref that is not there.
        said: String,
    },
    NotAdded(PathBuf),
    Io {
        what: String,
        error: io::Error,
    },
    /// The job's cap passed while it prepared.
    Overran,
}

/// What a value from the wire names on the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    Repo,
    Key,
    Nest,
    Token,
    GitDb,
    /// A path inside a tree.
    Fresh,
}

impl Named {
    /// `value`, unless it names a path outside the place it is for: one path component, or a
    /// path inside the tree for one a seed starts without, which may not be the tree itself.
    pub fn check(self, value: &str) -> Result<(), PrepareError> {
        let placed = match self {
            Named::Fresh => {
                let parts: Vec<Component> = Path::new(value).components().collect();
                parts.iter().any(|c| matches!(c, Component::Normal(_)))
                    && parts
                        .iter()
                        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
            }
            _ => !value.is_empty() && !value.starts_with('.') && !value.contains('/'),
        };
        match placed && !value.contains('\0') {
            true => Ok(()),
            false => Err(PrepareError::Unplaced {
                value: value.to_string(),
                names: self,
            }),
        }
    }

    fn what(self) -> &'static str {
        match self {
            Named::Repo => "repo's name",
            Named::Key => "sent tree's key",
            Named::Nest => "pinned trees' nest",
            Named::Token => "lockfile's token",
            Named::GitDb => "git database's name",
            Named::Fresh => "path inside a tree",
        }
    }
}

impl PrepareError {
    pub fn exit(&self) -> Exit {
        match self {
            PrepareError::Unplaced { .. } => Exit::Refused,
            PrepareError::Io { error, .. }
                if matches!(
                    error.kind(),
                    io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
                ) =>
            {
                Exit::NoRoom
            }
            PrepareError::Overran => Exit::Overran,
            PrepareError::NoClone(_)
            | PrepareError::NoRef { .. }
            | PrepareError::NotAdded(_)
            | PrepareError::Io { .. } => Exit::Setup,
        }
    }

    pub fn io(what: &str, error: io::Error) -> PrepareError {
        PrepareError::Io {
            what: what.to_string(),
            error,
        }
    }
}

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PrepareError::Unplaced { value, names } => {
                writeln!(f, "dibs: {value:?} is no {}", names.what())
            }
            PrepareError::NoClone(source) => writeln!(f, "dibs: no clone at {}", source.display()),
            PrepareError::NoRef {
                repo,
                reference,
                said,
            } => {
                writeln!(f, "dibs: no such ref in {repo}: {reference}")?;
                if CREDENTIALS.iter().any(|c| said.contains(c)) {
                    writeln!(
                        f,
                        "  The fetch failed on credentials, so nothing here can see that remote: a private\n  \
                         repo is the usual reason, and the ref itself is probably fine.\n  \
                         Send your working tree instead, which fetches nothing:  {repo}@local"
                    )?;
                }
                Ok(())
            }
            PrepareError::NotAdded(worktree) => {
                writeln!(f, "dibs: could not add the worktree {}", worktree.display())
            }
            PrepareError::Io { what, error } => writeln!(f, "dibs: {what}: {error}"),
            PrepareError::Overran => {
                writeln!(
                    f,
                    "dibs: the job's --max passed before its tree was laid out"
                )
            }
        }
    }
}
