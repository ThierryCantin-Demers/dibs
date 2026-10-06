use super::error::RepoError;
use crate::{cli::RecipeCall, execution, paths::FileError};
use std::path::{Path, PathBuf};

/// `--root`, or else where checkouts are looked up by default.
pub fn root_of(args: &RecipeCall) -> Result<PathBuf, RepoError> {
    match &args.root {
        Some(root) => Ok(root.clone()),
        None => repo_root(),
    }
}

/// Where a bare repo name is looked up. Everyone lays their checkouts out differently, so
/// this is only a starting guess: DIBS_ROOT, then --root, then the directory you are in.
pub fn repo_root() -> Result<PathBuf, RepoError> {
    if let Some(r) = std::env::var_os("DIBS_ROOT").filter(|r| !r.is_empty()) {
        return Ok(PathBuf::from(r));
    }
    // A fresh non-interactive shell has no DIBS_ROOT, since it lives in the user's fish
    // config, so the inventory file may carry it: `root = "/home/me/prog"` at the top level.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    Ok(crate::fleet::inventory()?
        .and_then(|i| i.root(home.as_deref()))
        .unwrap_or_else(|| PathBuf::from(".")))
}

pub fn resolve_repo(repo: &str, root: &Path) -> Result<PathBuf, RepoError> {
    let direct = PathBuf::from(repo);
    if direct.join(".dibs.toml").exists() || direct.join(".git").exists() {
        return canon(direct);
    }
    // A path into a subdirectory has no .git of its own, and `.` would otherwise join the root.
    if direct.is_absolute() || direct.starts_with(".") || direct.starts_with("..") {
        return execution::toplevel(&direct).ok_or_else(|| RepoError::NoCheckout(repo.to_string()));
    }
    // Inside a worktree of the named repo, or one by that directory name, `@local` means that
    // tree, and the clone under the root would otherwise be sent in its place without a word.
    let here = std::env::current_dir()
        .ok()
        .and_then(|d| execution::toplevel(&d));
    let named =
        |h: &PathBuf| execution::identity(h) == repo || h.file_name().is_some_and(|n| n == repo);
    if let Some(here) = here.filter(|h| !repo.contains('/') && named(h)) {
        return Ok(here);
    }
    let under = root.join(repo);
    if under.exists() {
        return canon(under);
    }
    Err(RepoError::NotFound {
        repo: repo.to_string(),
        root: root.to_path_buf(),
    })
}

pub fn canon(p: PathBuf) -> Result<PathBuf, RepoError> {
    Ok(p.canonicalize().map_err(FileError::at(&p))?)
}
