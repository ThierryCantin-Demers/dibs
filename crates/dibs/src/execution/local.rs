//! A tree sent from this computer: the repo it belongs to, the key its tree and cache are kept
//! under on the machine, the hash of what it holds, and a ref checked out here because the
//! machine cannot fetch it.

use crate::{
    execution::{
        error::{ArmError, CheckoutError},
        jobs::{JobRequest, Jobs, Reported},
    },
    git::{Git, GitError},
    paths::{FileError, Paths},
};
use dibs_format::{Hex, Moment, Span, wire};
use dibs_runner::shared::SharedFile;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// A checkout on this computer, asked which repo it is and what its refs name.
#[derive(Clone, Copy)]
pub struct Repo<'a>(pub &'a Path);

impl Repo<'_> {
    /// The repo a checkout belongs to rather than the folder it sits in. A worktree is named after
    /// its branch, and that name finds no recipes, no clone on the machine and no build cache.
    pub fn identity(self) -> String {
        let dir = self.0;
        let folder = |p: &Path| p.file_name().and_then(|s| s.to_str()).map(str::to_string);
        let fallback = folder(dir).unwrap_or_else(|| "repo".into());
        // A directory inside some other repo is not that repo, so only a checkout's own top level
        // is asked.
        if self.toplevel() != dir.canonicalize().ok() {
            return fallback;
        }
        // A clone of a checkout here, such as a scratch copy for a review, is that checkout's
        // repo; a bare origin stands in for a host, and its name is not the repo's.
        let origin = self.checkout_origin();
        let own = origin.as_deref().unwrap_or(dir);
        let Ok(common) = Git(own).run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        else {
            return fallback;
        };
        let common = PathBuf::from(common.trim());
        let named = if common.file_name().and_then(|s| s.to_str()) == Some(".git") {
            common.parent().and_then(folder)
        } else {
            folder(&common).map(|n| n.trim_end_matches(".git").to_string())
        };
        named.filter(|n| !n.is_empty()).unwrap_or(fallback)
    }

    /// The checkout on this computer that `origin` points at, if it is one.
    fn checkout_origin(self) -> Option<PathBuf> {
        let url = Git(self.0)
            .run(&["config", "--get", "remote.origin.url"])
            .ok()?;
        let url = url.trim();
        let path = self
            .0
            .join(url.strip_prefix("file://").unwrap_or(url))
            .canonicalize()
            .ok()?;
        Repo(&path).toplevel().filter(|top| *top == path)
    }

    /// The checkout this directory is inside, if any.
    pub fn toplevel(self) -> Option<PathBuf> {
        let top = Git(self.0).run(&["rev-parse", "--show-toplevel"]).ok()?;
        PathBuf::from(top.trim()).canonicalize().ok()
    }

    /// The folder a worktree sits in, which says which line of work a run came from. Only for the
    /// record: the label, the recipes and the caches all go by `identity`.
    pub fn variant(self, identity: &str) -> Option<String> {
        let folder = self.0.file_name()?.to_str()?;
        (folder != identity).then(|| folder.to_string())
    }

    /// The commit `name` is here, in full.
    pub fn commit(self, name: &str) -> Result<String, ArmError> {
        Git(self.0)
            .run(&["rev-parse", "--verify", "-q", &format!("{name}^{{commit}}")])
            .map(|s| s.trim().to_string())
            .map_err(|_| ArmError::NoCommit {
                name: name.to_string(),
                dir: self.0.to_path_buf(),
            })
    }

    /// Why a machine, which holds no credentials, cannot fetch `sha` from this checkout's origin,
    /// or None when it can. A commit on no branch of origin here is taken as never pushed: sending
    /// one that was costs a transfer, where fetching one that was not fails the run.
    pub fn unfetchable(self, sha: &str) -> Option<&'static str> {
        let on = Git(self.0).run(&[
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
        let url = Git(self.0).run(&["remote", "get-url", "origin"]).ok()?;
        private(url.trim())
            .then_some("its remote needs credentials, which the machines do not hold")
    }

    /// The lockfile of the tree here, or of a ref in its history.
    pub fn lockfile(self, reference: Option<&str>) -> Option<String> {
        match reference {
            None => std::fs::read_to_string(self.0.join("Cargo.lock")).ok(),
            Some(r) => Git(self.0).run(&["show", &format!("{r}:Cargo.lock")]).ok(),
        }
    }
}

/// How a local tree is sent. `--checksum` without `--times` is what the seed relies on: a file
/// whose bytes match is left alone with the time it was copied with, and any other is rewritten
/// and takes the current time.
/// The marker is excluded so `--delete` leaves it, or collection could never date the tree.
const SYNC_ARGS: &[&str] = &[
    "-rlpgo",
    "--checksum",
    "--no-times",
    "--delete",
    "--exclude=.git",
    "--exclude=/.dibs-used",
    "--filter=:- .gitignore",
];

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

impl Local {
    pub fn of(dir: &Path) -> Result<Local, GitError> {
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
            h.finalize().hex()
        );
        let mut k = Sha256::new();
        k.update(dir.as_os_str().as_encoded_bytes());
        Ok(Local {
            key: format!("{:.10}", k.finalize().hex()),
            content,
            dirty,
        })
    }

    /// Prepares the worktree and sends the tree at `from` into it, as one job under one lock.
    ///
    /// `--no-times` is the load-bearing option and it is not tidiness. rsync's `-a` implies `-t`,
    /// which is right for a transfer and wrong for sources about to be compiled: files that arrive
    /// carrying an older mtime than the artifacts already beside them leave cargo with nothing to
    /// do, so the build finishes in a fraction of a second and the previous binary is what gets
    /// measured. It reads exactly like a fast incremental build. `--checksum` is what makes
    /// dropping `-t` affordable, because without it every destination mtime differs on the next
    /// pass and the whole tree goes again each time.
    ///
    /// The filter follows the repo's own ignore rules, so a target directory or an editor's
    /// droppings never make the trip, and `--delete` means a file deleted locally stops existing
    /// there too rather than going on compiling.
    pub fn send(
        &self,
        from: &Path,
        backend: &Jobs,
        req: &JobRequest,
        on_prepared: &mut dyn FnMut(&wire::Prepared),
    ) -> Reported {
        let args: Vec<String> = SYNC_ARGS
            .iter()
            .map(|a| a.to_string())
            .chain([
                format!("{}/", from.display()),
                format!(":local-{}/", self.key),
            ])
            .collect();
        backend.sync(req, &args, on_prepared)
    }
}

/// A commit checked out here to be sent like a local tree: one the machine cannot fetch, or the
/// base of a comparison against the local tree, which is sent like its tip.
///
/// A few checkouts per repo, each a clone that borrows this checkout's objects, so nothing is
/// copied but files and no worktree is registered in the checkout. Its machine tree is keyed by
/// the commit rather than by the path, so two commits never build in one tree. Each is locked
/// until it has been sent, which is why a comparison of two such commits takes two.
pub struct Checkout {
    pub dir: PathBuf,
    pub sha: String,
    pub key: String,
    pub lock: Option<std::fs::File>,
    /// Why the machine is not left to fetch it.
    pub why: Option<&'static str>,
}

impl Checkout {
    pub fn of(
        dir: &Path,
        identity: &str,
        sha: &str,
        why: Option<&'static str>,
    ) -> Result<Checkout, CheckoutError> {
        Checkout::under(
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

    fn under(
        root: &Path,
        dir: &Path,
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
            key: format!("{:.10}", k.finalize().hex()),
            lock: Some(lock),
            why,
        })
    }

    pub fn short_sha(&self) -> &str {
        &self.sha[..12.min(self.sha.len())]
    }

    /// `, <note>, sent from <from> since <why>`, as much of it as there is, to follow the commit.
    pub fn sent_from(&self, note: Option<&str>, from: &str) -> String {
        format!(
            "{}, sent from {from}{}",
            note.map(|n| format!(", {n}")).unwrap_or_default(),
            self.why.map(|w| format!(" since {w}")).unwrap_or_default()
        )
    }

    pub fn local(&self) -> Result<Local, GitError> {
        Ok(Local {
            key: self.key.clone(),
            ..Local::of(&self.dir)?
        })
    }
}

/// A ref as this checkout sees it, for a run that may have to send it.
pub struct Fetched {
    pub commit: String,
    /// The name it was found under.
    pub seen: String,
    /// Why it must be sent rather than fetched by name.
    pub ahead: Option<&'static str>,
}

impl Fetched {
    /// `name` as this checkout sees it. That is the remote-tracking ref, which a local branch of
    /// that name may be behind, unless the local branch has commits origin lacks: fetching it would
    /// take origin's.
    pub fn of(dir: &Path, name: &str) -> Option<Fetched> {
        let tracking = format!("origin/{name}");
        let Ok(pushed) = Repo(dir).commit(&tracking) else {
            return Repo(dir).commit(name).ok().map(|commit| Fetched {
                commit,
                seen: name.to_string(),
                ahead: None,
            });
        };
        match Repo(dir).commit(&format!("refs/heads/{name}")) {
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

    /// `name` as `of` finds it, after asking origin for it when this checkout has none.
    pub fn fetching(dir: &Path, name: &str) -> Option<Fetched> {
        Fetched::of(dir, name).or_else(|| {
            eprintln!(
                "dibs: {} has no {name}; fetching it from origin",
                dir.display()
            );
            Git(dir).run(&["fetch", "--quiet", "origin", name]).ok()?;
            Fetched::of(dir, name)
        })
    }
}

/// A remote that refuses an anonymous read. Remembered for a week, since asking costs a round
/// trip to the host; a remote that cannot be asked counts as public, which is what it was taken
/// for before anything asked.
fn private(url: &str) -> bool {
    const WEEK: u64 = 7 * Span::DAY.0;
    let Some(url) = anonymous_url(url) else {
        return false;
    };
    let Some(file) = Paths::from_env().remotes() else {
        return false;
    };
    let now = Moment::epoch_now();
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
        assert_eq!(Repo(&home.join("topk-branch")).identity(), "cubek");
        assert_eq!(Repo(&main).identity(), "cubek");
        assert_eq!(Repo(&main.join("inner")).identity(), "inner");
        assert_eq!(Repo(&home.join("loose")).identity(), "loose");
        assert_eq!(
            Repo(&home.join("topk-branch")).variant("cubek").as_deref(),
            Some("topk-branch")
        );
        assert_eq!(Repo(&main).variant("cubek"), None);
        let _ = std::fs::remove_dir_all(&home);
    }

    // Both arms of an A/B shared one target directory, and cargo does not isolate same-name
    // packages by source path, so one arm ran the other's binary.
    #[test]
    fn two_local_trees_never_share_a_cache() {
        let (a, b) = (tmp("a"), tmp("b"));
        repo(&a);
        repo(&b);
        assert_ne!(Local::of(&a).unwrap().key, Local::of(&b).unwrap().key);
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
            Repo(&repo).commit("HEAD~1").unwrap(),
            Repo(&repo).commit("HEAD").unwrap(),
        );
        let root = scratch.join("sent/repo");
        let at = |c: &Checkout| {
            Git(&c.dir)
                .run(&["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .to_string()
        };
        let a = Checkout::under(&root, &repo, "repo", &one, None).unwrap();
        let b = Checkout::under(&root, &repo, "repo", &two, None).unwrap();
        assert_eq!((at(&a), at(&b)), (one, two.clone()));
        assert_ne!(a.dir, b.dir, "the first is held until it is sent");
        let c = Checkout::under(&root, &repo, "repo", &two, None).unwrap();
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
        let before = Local::of(&a).unwrap();
        assert!(!before.dirty);
        std::fs::write(a.join("a.rs"), "fn main() { let _ = 1; }\n").unwrap();
        let after = Local::of(&a).unwrap();
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
        let before = Local::of(&a).unwrap().content;
        std::fs::write(a.join("b.rs"), "fn other() {}\n").unwrap();
        assert_ne!(before, Local::of(&a).unwrap().content);
    }

    // And an ignored one must not, because it does not make the trip either.
    #[test]
    fn an_ignored_file_does_not() {
        let a = tmp("ignored");
        repo(&a);
        let before = Local::of(&a).unwrap().content;
        std::fs::create_dir_all(a.join("target")).unwrap();
        std::fs::write(a.join("target/big.rlib"), "artifact\n").unwrap();
        assert_eq!(before, Local::of(&a).unwrap().content);
    }
}
