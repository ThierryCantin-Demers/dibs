use std::{
    fmt, io,
    path::{Path, PathBuf},
    process::Command,
};

/// git, run in one checkout.
pub struct Git<'a>(pub &'a Path);

/// Why a git command gave nothing to use.
#[derive(Debug)]
pub enum GitError {
    Unstarted {
        args: String,
        error: io::Error,
    },
    Failed {
        args: String,
        dir: PathBuf,
        said: String,
    },
}

impl Git<'_> {
    /// What the command printed, or what git said when it failed.
    pub fn run(&self, args: &[&str]) -> Result<String, GitError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.0)
            .args(args)
            .output()
            .map_err(|error| GitError::Unstarted {
                args: args.join(" "),
                error,
            })?;
        if !out.status.success() {
            return Err(GitError::Failed {
                args: args.join(" "),
                dir: self.0.to_path_buf(),
                said: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitError::Unstarted { args, error } => write!(f, "git {args}: {error}"),
            GitError::Failed { args, dir, said } => {
                write!(f, "git {args} in {}: {said}", dir.display())
            }
        }
    }
}
