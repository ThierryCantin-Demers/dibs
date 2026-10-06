//! A tree sent from this computer: the repo it belongs to, the key its tree and cache are kept
//! under on the machine, the hash of what it holds, and a ref checked out here because the
//! machine cannot fetch it.

use crate::{
    execution::{build::hex, error::CheckoutError, refs::commit},
    git::{Git, GitError},
    paths::{FileError, Paths},
};
use dibs_runner::shared::SharedFile;
use sha2::{Digest, Sha256};

/// What a local tree is, for a run that was never pushed.
///
/// The cache key is the tree's own path and nothing else, deliberately. Keying it on content
/// would give every edit a cold build, which is the whole reason someone hand-rolls this; keying
/// it on the repo, as a fetched ref does, would put both arms of an A/B in one target directory,
/// and cargo does not isolate same-name packages by source path, so one arm ends up running the
/// other's binary. One directory per local tree is the only key that is both warm and separate.
///
/// `content` is separate and is for the record rather than for the cache: two runs of one label
/// are the same measurement only if it matches.
pub struct Local {
    pub key: String,
    pub content: String,
    pub dirty: bool,
}

/// The repo a checkout belongs to rather than the folder it sits in. A worktree is named after
/// its branch, and that name finds no recipes, no clone on the machine and no build cache.
pub fn identity(dir: &std::path::Path) -> String {
    let folder = |p: &std::path::Path| p.file_name().and_then(|s| s.to_str()).map(str::to_string);
    let fallback = folder(dir).unwrap_or_else(|| "repo".into());
    // A directory inside some other repo is not that repo, so only a checkout's own top level
    // is asked.
    if toplevel(dir) != dir.canonicalize().ok() {
        return fallback;
    }
    let Ok(common) = Git(dir).run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
    else {
        return fallback;
    };
    let common = std::path::PathBuf::from(common.trim());
    let named = if common.file_name().and_then(|s| s.to_str()) == Some(".git") {
        common.parent().and_then(folder)
    } else {
        folder(&common).map(|n| n.trim_end_matches(".git").to_string())
    };
    named.filter(|n| !n.is_empty()).unwrap_or(fallback)
}

/// The checkout `dir` is inside, if any.
pub fn toplevel(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let top = Git(dir).run(&["rev-parse", "--show-toplevel"]).ok()?;
    std::path::PathBuf::from(top.trim()).canonicalize().ok()
}

/// The folder a worktree sits in, which says which line of work a run came from. Only for the
/// record: the label, the recipes and the caches all go by `identity`.
pub fn variant(dir: &std::path::Path, identity: &str) -> Option<String> {
    let folder = dir.file_name()?.to_str()?;
    (folder != identity).then(|| folder.to_string())
}

pub fn local(dir: &std::path::Path) -> Result<Local, GitError> {
    let head = Git(dir)
        .run(&["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string();
    // Tracked and untracked-but-not-ignored, which is the same set the sync carries, so the
    // hash describes what was actually built rather than what was committed.
    let list = Git(dir).run(&["ls-files", "-co", "--exclude-standard", "-z"])?;
    let mut h = Sha256::new();
    let mut dirty = false;
    for rel in list.split('\0').filter(|s| !s.is_empty()) {
        h.update(rel.as_bytes());
        h.update([0]);
        if let Ok(b) = std::fs::read(dir.join(rel)) {
            h.update(b.len().to_le_bytes());
            h.update(&b);
        }
    }
    if !Git(dir).run(&["status", "--porcelain"])?.trim().is_empty() {
        dirty = true;
    }
    let content = format!(
        "{head}{}-{:.12}",
        if dirty { "+dirty" } else { "" },
        hex(&h.finalize())
    );
    let mut k = Sha256::new();
    k.update(dir.as_os_str().as_encoded_bytes());
    Ok(Local {
        key: format!("{:.10}", hex(&k.finalize())),
        content,
        dirty,
    })
}

/// A commit checked out here to be sent like a local tree: one the machine cannot fetch, or the
/// base of a comparison against the local tree, which is sent like its tip.
///
/// A few checkouts per repo, each a clone that borrows this checkout's objects, so nothing is
/// copied but files and no worktree is registered in the checkout. Its machine tree is keyed by
/// the commit rather than by the path, so two commits never build in one tree. Each is locked
/// until it has been sent, which is why a comparison of two such commits takes two.
pub struct Checkout {
    pub dir: std::path::PathBuf,
    pub sha: String,
    pub key: String,
    pub lock: Option<std::fs::File>,
    /// Why the machine is not left to fetch it.
    pub why: Option<&'static str>,
}

impl Checkout {
    pub fn local(&self) -> Result<Local, GitError> {
        Ok(Local {
            key: self.key.clone(),
            ..local(&self.dir)?
        })
    }
}

pub fn checkout(
    dir: &std::path::Path,
    identity: &str,
    sha: &str,
    why: Option<&'static str>,
) -> Result<Checkout, CheckoutError> {
    checkout_in(
        &Paths::from_env()
            .sent()
            .ok_or(CheckoutError::NoHome)?
            .join(identity),
        dir,
        identity,
        sha,
        why,
    )
}

fn checkout_in(
    root: &std::path::Path,
    dir: &std::path::Path,
    identity: &str,
    sha: &str,
    why: Option<&'static str>,
) -> Result<Checkout, CheckoutError> {
    std::fs::create_dir_all(root).map_err(FileError::at(root))?;
    let (slot, lock) = (0u32..)
        .find_map(|n| {
            let path = root.join(format!("{n}.lock"));
            let taken = std::fs::File::create(&path)
                .map_err(std::fs::TryLockError::Error)
                .and_then(|f| f.try_lock().map(|()| f));
            match taken {
                Ok(f) => Some(Ok((n.to_string(), f))),
                Err(std::fs::TryLockError::WouldBlock) => None,
                Err(std::fs::TryLockError::Error(e)) => Some(Err(FileError::new(&path, e))),
            }
        })
        .expect("an unbounded range")?;
    let checkout = root.join(&slot);
    if !checkout.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&checkout);
        let from = dir
            .to_str()
            .ok_or_else(|| CheckoutError::Unnamed(dir.to_path_buf()))?;
        Git(root).run(&["clone", "--quiet", "--shared", "--no-checkout", from, &slot])?;
    }
    Git(&checkout).run(&["checkout", "--quiet", "--detach", "--force", sha])?;
    Git(&checkout).run(&["clean", "-fdxq"])?;
    let mut k = Sha256::new();
    k.update(format!("base\0{identity}\0{sha}"));
    Ok(Checkout {
        dir: checkout,
        sha: sha.to_string(),
        key: format!("{:.10}", hex(&k.finalize())),
        lock: Some(lock),
        why,
    })
}

/// A ref as this checkout sees it, for a run that may have to send it.
pub struct Fetched {
    pub commit: String,
    /// The name it was found under.
    pub seen: String,
    /// Why it must be sent rather than fetched by name.
    pub ahead: Option<&'static str>,
}

/// `name` as this checkout sees it. That is the remote-tracking ref, which a local branch of that
/// name may be behind, unless the local branch has commits origin lacks: fetching it would take
/// origin's.
pub fn as_fetched(dir: &std::path::Path, name: &str) -> Option<Fetched> {
    let tracking = format!("origin/{name}");
    let Ok(pushed) = commit(dir, &tracking) else {
        return commit(dir, name).ok().map(|commit| Fetched {
            commit,
            seen: name.to_string(),
            ahead: None,
        });
    };
    match commit(dir, &format!("refs/heads/{name}")) {
        Ok(own)
            if Git(dir)
                .run(&["merge-base", "--is-ancestor", &own, &pushed])
                .is_err() =>
        {
            Some(Fetched {
                commit: own,
                seen: name.to_string(),
                ahead: Some("origin's branch lacks it"),
            })
        }
        _ => Some(Fetched {
            commit: pushed,
            seen: tracking,
            ahead: None,
        }),
    }
}

/// Why a machine, which holds no credentials, cannot fetch `sha` from this checkout's origin, or
/// None when it can. A commit on no branch of origin here is taken as never pushed: sending one
/// that was costs a transfer, where fetching one that was not fails the run.
pub fn unfetchable(dir: &std::path::Path, sha: &str) -> Option<&'static str> {
    let on = Git(dir).run(&[
        "for-each-ref",
        "--count=1",
        "--contains",
        sha,
        "--format=%(refname)",
        "refs/remotes/origin/",
    ]);
    if on.map_or(true, |r| r.trim().is_empty()) {
        return Some("it was never pushed");
    }
    let url = Git(dir).run(&["remote", "get-url", "origin"]).ok()?;
    private(url.trim()).then_some("its remote needs credentials, which the machines do not hold")
}

/// A remote that refuses an anonymous read. Remembered for a week, since asking costs a round
/// trip to the host; a remote that cannot be asked counts as public, which is what it was taken
/// for before anything asked.
fn private(url: &str) -> bool {
    const WEEK: u64 = 7 * 24 * 3600;
    let Some(url) = anonymous_url(url) else {
        return false;
    };
    let Some(file) = Paths::from_env().remotes() else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let known = std::fs::read_to_string(&file).unwrap_or_default();
    let seen = known.lines().rev().find_map(|l| {
        let mut f = l.split('\t');
        let (u, what, when) = (f.next()?, f.next()?, f.next()?.parse::<u64>().ok()?);
        (u == url && now.saturating_sub(when) < WEEK).then_some(what == "private")
    });
    if let Some(p) = seen {
        return p;
    }
    let Some(p) = refuses_anonymous(&url) else {
        return false;
    };
    let _ = SharedFile { path: &file }.rewrite(|known| {
        let mut kept: String = known
            .lines()
            .filter(|l| l.split('\t').next() != Some(url.as_str()))
            .map(|l| format!("{l}\n"))
            .collect();
        kept += &format!("{url}\t{}\t{now}\n", if p { "private" } else { "public" });
        Some(kept)
    });
    p
}

/// The https form of a remote, which is how a machine without keys would have to read it.
fn anonymous_url(url: &str) -> Option<String> {
    let (host, path) = if let Some(rest) = url.strip_prefix("https://") {
        rest.split_once('/')?
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next()?, path)
    } else if !url.contains("://") && url.contains('@') {
        url.split_once(':')?
    } else {
        return None;
    };
    let host = host.rsplit('@').next()?;
    (!host.is_empty() && !path.is_empty()).then(|| format!("https://{host}/{path}"))
}

/// Whether the remote refused a read without credentials, or None when it could not be asked.
fn refuses_anonymous(url: &str) -> Option<bool> {
    let child = std::process::Command::new("git")
        .args([
            "-c",
            "credential.helper=",
            "ls-remote",
            "--exit-code",
            url,
            "HEAD",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env_remove("SSH_ASKPASS")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output()));
    let Ok(Ok(out)) = rx.recv_timeout(std::time::Duration::from_secs(15)) else {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
        return None;
    };
    let err = String::from_utf8_lossy(&out.stderr);
    match out.status.success() {
        true => Some(false),
        false => [
            "Authentication failed",
            "could not read Username",
            "Repository not found",
        ]
        .iter()
        .any(|m| err.contains(m))
        .then_some(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn repo(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        for a in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.invalid"],
            vec!["config", "user.name", "t"],
        ] {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(a)
                .status()
                .unwrap();
        }
        std::fs::write(dir.join("a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "target\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["add", "-A"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-qm", "one"])
            .status()
            .unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dibs-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_worktree_is_the_repo_it_belongs_to() {
        let home = std::env::temp_dir().join(format!("dibs-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let main = home.join("cubek");
        std::fs::create_dir_all(main.join("inner")).unwrap();
        std::fs::create_dir_all(home.join("loose")).unwrap();
        let sh = |cmd: &str| {
            let o = std::process::Command::new("bash")
                .arg("-c")
                .arg(cmd)
                .current_dir(&home)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "{cmd}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        };
        sh(
            "git -C cubek init -q && git -C cubek -c user.email=a@b -c user.name=t commit -q --allow-empty -m one",
        );
        sh("git -C cubek worktree add -q ../topk-branch");
        assert_eq!(identity(&home.join("topk-branch")), "cubek");
        assert_eq!(identity(&main), "cubek");
        assert_eq!(identity(&main.join("inner")), "inner");
        assert_eq!(identity(&home.join("loose")), "loose");
        assert_eq!(
            variant(&home.join("topk-branch"), "cubek").as_deref(),
            Some("topk-branch")
        );
        assert_eq!(variant(&main, "cubek"), None);
        let _ = std::fs::remove_dir_all(&home);
    }

    // Both arms of an A/B shared one target directory, and cargo does not isolate same-name
    // packages by source path, so one arm ran the other's binary.
    #[test]
    fn two_local_trees_never_share_a_cache() {
        let (a, b) = (tmp("a"), tmp("b"));
        repo(&a);
        repo(&b);
        assert_ne!(local(&a).unwrap().key, local(&b).unwrap().key);
    }

    #[test]
    fn a_remote_is_asked_over_https_as_a_machine_without_keys_would_read_it() {
        for (url, https) in [
            (
                "https://github.com/o/r.git",
                Some("https://github.com/o/r.git"),
            ),
            (
                "https://user@github.com/o/r",
                Some("https://github.com/o/r"),
            ),
            ("git@github.com:o/r.git", Some("https://github.com/o/r.git")),
            (
                "ssh://git@github.com:22/o/r.git",
                Some("https://github.com/o/r.git"),
            ),
            ("/srv/git/r.git", None),
            ("file:///srv/git/r.git", None),
        ] {
            assert_eq!(anonymous_url(url).as_deref(), https, "{url}");
        }
    }

    #[test]
    fn two_commits_sent_at_once_take_a_checkout_each() {
        let scratch = tmp("checkouts");
        let repo = scratch.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let commit_empty = |m: &str| {
            Git(&repo)
                .run(&[
                    "-c",
                    "user.email=a@b",
                    "-c",
                    "user.name=t",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    m,
                ])
                .unwrap()
        };
        Git(&repo).run(&["init", "-q"]).unwrap();
        commit_empty("one");
        commit_empty("two");
        let (one, two) = (
            commit(&repo, "HEAD~1").unwrap(),
            commit(&repo, "HEAD").unwrap(),
        );
        let root = scratch.join("sent/repo");
        let at = |c: &Checkout| {
            Git(&c.dir)
                .run(&["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .to_string()
        };
        let a = checkout_in(&root, &repo, "repo", &one, None).unwrap();
        let b = checkout_in(&root, &repo, "repo", &two, None).unwrap();
        assert_eq!((at(&a), at(&b)), (one, two.clone()));
        assert_ne!(a.dir, b.dir, "the first is held until it is sent");
        let c = checkout_in(&root, &repo, "repo", &two, None).unwrap();
        assert!(c.dir != a.dir && c.dir != b.dir, "nor is the second");
        assert_eq!(
            c.key, b.key,
            "a commit's tree on the machine is the same whichever checkout sent it"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // Keyed on content instead, every edit would be a cold build, which is the reason the
    // hand-rolled version pointed both arms at one directory in the first place.
    #[test]
    fn editing_a_tree_keeps_its_cache_and_changes_what_the_record_says() {
        let a = tmp("edit");
        repo(&a);
        let before = local(&a).unwrap();
        assert!(!before.dirty);
        std::fs::write(a.join("a.rs"), "fn main() { let _ = 1; }\n").unwrap();
        let after = local(&a).unwrap();
        assert_eq!(before.key, after.key);
        assert_ne!(before.content, after.content);
        assert!(after.dirty);
    }

    // The hash has to cover a file git has never seen, or a new source file is invisible to
    // the record while being compiled by the build.
    #[test]
    fn an_untracked_source_file_changes_the_content_hash() {
        let a = tmp("untracked");
        repo(&a);
        let before = local(&a).unwrap().content;
        std::fs::write(a.join("b.rs"), "fn other() {}\n").unwrap();
        assert_ne!(before, local(&a).unwrap().content);
    }

    // And an ignored one must not, because it does not make the trip either.
    #[test]
    fn an_ignored_file_does_not() {
        let a = tmp("ignored");
        repo(&a);
        let before = local(&a).unwrap().content;
        std::fs::create_dir_all(a.join("target")).unwrap();
        std::fs::write(a.join("target/big.rlib"), "artifact\n").unwrap();
        assert_eq!(before, local(&a).unwrap().content);
    }
}
