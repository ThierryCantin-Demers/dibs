use crate::worktree;
use dibs::cli::RecipeCall;
use std::path::{Path, PathBuf};

/// `--root`, or else where checkouts are looked up by default.
pub(crate) fn root_of(args: &RecipeCall) -> PathBuf {
    args.root.clone().unwrap_or_else(repo_root)
}

/// Where a bare repo name is looked up. Everyone lays their checkouts out differently, so
/// this is only a starting guess: DIBS_ROOT, then --root, then the directory you are in.
pub(crate) fn repo_root() -> PathBuf {
    if let Some(r) = std::env::var_os("DIBS_ROOT").filter(|r| !r.is_empty()) {
        return PathBuf::from(r);
    }
    // A fresh non-interactive shell has no DIBS_ROOT, since it lives in the user's fish
    // config, so the inventory file may carry it: `root = "/home/me/prog"` at the top level.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    crate::fleet::inventory()
        .and_then(|i| i.root(home.as_deref()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn resolve_repo(repo: &str, root: &Path) -> Result<PathBuf, String> {
    let direct = PathBuf::from(repo);
    if direct.join(".dibs.toml").exists() || direct.join(".git").exists() {
        return canon(direct);
    }
    // A path into a subdirectory has no .git of its own, and `.` would otherwise join the root.
    if direct.is_absolute() || direct.starts_with(".") || direct.starts_with("..") {
        return worktree::toplevel(&direct).ok_or_else(|| {
            format!(
                "'{repo}' is in no git checkout and has no .dibs.toml, so there is no tree to send"
            )
        });
    }
    // Inside a worktree of the named repo, or one by that directory name, `@local` means that
    // tree, and the clone under the root would otherwise be sent in its place without a word.
    let here = std::env::current_dir()
        .ok()
        .and_then(|d| worktree::toplevel(&d));
    let named =
        |h: &PathBuf| worktree::identity(h) == repo || h.file_name().is_some_and(|n| n == repo);
    if let Some(here) = here.filter(|h| !repo.contains('/') && named(h)) {
        return Ok(here);
    }
    let under = root.join(repo);
    if under.exists() {
        return canon(under);
    }
    Err(format!(
        "no repo at '{repo}' and none at {}/{repo}.\n  A bare name is looked up under DIBS_ROOT, then --root, then the `root` key of\n  ~/.config/dibs/machines.toml, then the current directory. Give a path, or set one of those.",
        root.display()
    ))
}

pub(crate) fn canon(p: PathBuf) -> Result<PathBuf, String> {
    p.canonicalize()
        .map_err(|e| format!("{}: {e}", p.display()))
}
