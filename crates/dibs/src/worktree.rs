//! Getting to the point where cargo can be run at all.
//!
//! In the log this replaces, 107 of 179 jobs named a hand-written worktree path and only 9
//! began with `cargo`: nearly all the length of a typical command was fetching a ref, adding a
//! worktree, and arranging a build cache, with six agents each having invented their own
//! version including whether to `mv` or `cp -a` a sibling's target directory.
//!
//! So dibs owns the layout and nothing is asked to follow a convention by hand. The setup runs
//! on the machine under the shared lock, because it is a fetch and a checkout: work that
//! tolerates neighbours perfectly and must never hold the exclusive lock.

use crate::{git::Git, lockfile::Package};
use dibs::paths::Paths;
use dibs_runner::shared::SharedFile;
use sha2::{Digest, Sha256};

/// A build step wrapped so that, once it exits 0, what its own prepare staged joins the target's
/// record. The record is a union: old artifacts stay when a tree moves on, so a revision built
/// last week still counts. The lock is for two builds of one target finishing together.
pub fn recording(run: &str, token: &str) -> String {
    format!(
        r#"( {run} ); rc=$?
staged="$CARGO_TARGET_DIR/.dibs-packages.pending.{token}"
if [ "$rc" = 0 ] && [ -s "$staged" ]; then
    (
        flock 9
        {{ cat "$CARGO_TARGET_DIR/.dibs-packages" 2>/dev/null || true; cat "$staged"; }} | LC_ALL=C sort -u > "$CARGO_TARGET_DIR/.dibs-packages.new.$$" &&
            mv "$CARGO_TARGET_DIR/.dibs-packages.new.$$" "$CARGO_TARGET_DIR/.dibs-packages" && rm -f "$staged"
    ) 9>"$CARGO_TARGET_DIR/.dibs-packages.lock"
fi
exit $rc"#
    )
}

/// A build step that claims its target for this tree. Cargo judges a crate fresh when its sources
/// are older than its last compile, so a tree checked out before another tree built into a shared
/// target is handed that tree's artifacts unless its sources are dated after them.
pub fn claiming(run: &str) -> String {
    format!(
        r#"(
    flock 8
    if [ "$(cat "$CARGO_TARGET_DIR/.dibs-tree" 2>/dev/null)" != "$PWD" ]; then
        find . -name .git -prune -o -type f -exec touch -c -- {{}} +
        printf '%s\n' "$PWD" > "$CARGO_TARGET_DIR/.dibs-tree"
        echo "dibs: this tree did not make the last build in $CARGO_TARGET_DIR, so cargo rebuilds its crates" >&2
    fi
) 8>"$CARGO_TARGET_DIR/.dibs-tree.lock"
{run}"#
    )
}

/// A measured step refuses a target another tree has claimed since this recipe built: the binary
/// there may be that tree's. It is read under the exclusive lock, so nothing builds in between.
pub fn checked(run: &str) -> String {
    format!(
        r#"if [ "$(cat "$CARGO_TARGET_DIR/.dibs-tree" 2>/dev/null)" != "$PWD" ]; then
    echo DIBS-REFUSED
    echo "dibs: refused to measure: another tree built into $CARGO_TARGET_DIR after this one did, so the binary there may be that tree's." >&2
    echo "  Run it again, which rebuilds this tree first, or pass --anyway to measure what is there." >&2
    exit 78
fi
{run}"#
    )
}

/// What a cargo build's artifacts depend on besides the lockfile: toolchain, profile, target,
/// feature flags and RUSTFLAGS. Two builds that differ here share almost nothing, so it is folded
/// into every line of the package list and they never match each other. `None` for anything
/// that is not a cargo command producing artifacts, `check` and `clippy` included, since those
/// leave metadata a build cannot use.
pub fn build_signature(run: &str) -> Option<String> {
    let unquote = |w: &str| w.trim_matches(|c| c == '"' || c == '\'').to_string();
    let run = run.replace("&&", "\n").replace("||", "\n");
    for segment in run.split([';', '|', '\n']) {
        let mut words = segment.split_whitespace().map(unquote).peekable();
        let mut rustflags = String::new();
        while let Some(w) = words.peek() {
            if w == "env" {
                words.next();
            } else if !w.starts_with('-') && w.contains('=') {
                if let Some(v) = w.strip_prefix("RUSTFLAGS=") {
                    rustflags = v.to_string();
                }
                words.next();
            } else {
                break;
            }
        }
        if words
            .next()
            .as_deref()
            .map(|p| p.rsplit('/').next() == Some("cargo"))
            != Some(true)
        {
            continue;
        }
        let mut toolchain = String::new();
        let mut sub = words.next().unwrap_or_default();
        if let Some(t) = sub.strip_prefix('+') {
            toolchain = t.to_string();
            sub = words.next().unwrap_or_default();
        }
        if !matches!(
            sub.as_str(),
            "build" | "b" | "test" | "t" | "bench" | "run" | "r" | "nextest"
        ) {
            continue;
        }
        let words: Vec<String> = words.collect();
        let mut profile = if sub == "bench" {
            "release".to_string()
        } else {
            "dev".to_string()
        };
        let (mut target, mut features, mut flags) = (String::new(), Vec::new(), Vec::new());
        let mut i = 0;
        while i < words.len() {
            let w = words[i].as_str();
            let next = words.get(i + 1).cloned().unwrap_or_default();
            match w {
                "--" => break,
                "--release" | "-r" => profile = "release".into(),
                "--profile" => profile = next,
                "--target" => target = next,
                "--features" | "-F" => features.extend(next.split(',').map(str::to_string)),
                "--no-default-features" | "--all-features" => flags.push(w.to_string()),
                _ => {
                    if let Some(v) = w.strip_prefix("--profile=") {
                        profile = v.into();
                    } else if let Some(v) = w.strip_prefix("--target=") {
                        target = v.into();
                    } else if let Some(v) = w
                        .strip_prefix("--features=")
                        .or_else(|| w.strip_prefix("-F"))
                    {
                        features.extend(v.split(',').map(str::to_string));
                    }
                }
            }
            i += 1;
        }
        features.retain(|f: &String| !f.is_empty());
        features.sort();
        features.dedup();
        flags.sort();
        flags.dedup();
        return Some(format!(
            "{toolchain}|{profile}|{target}|{}|{}|{rustflags}",
            flags.join(" "),
            features.join(",")
        ));
    }
    None
}

/// A lockfile as the short sorted hashes the machine's seed and a target's record compare. A git
/// package gets a line of its own, since a revision is what most often differs and costs most.
/// Registry packages share 64 lines, one per bucket of their contents, so a lockfile of a
/// thousand crates is still a short list.
pub fn packages(lock: &str, signature: &str) -> Vec<String> {
    let short = |text: &str| {
        format!(
            "{:.12}",
            hex(&Sha256::digest(format!("{signature}\n{text}").as_bytes()))
        )
    };
    let mut lines = std::collections::BTreeSet::new();
    let mut buckets: Vec<Vec<String>> = vec![Vec::new(); 64];
    let mut take = |name: Option<String>, version: Option<String>, source: Option<String>| {
        let (Some(n), Some(v), Some(src)) = (name, version, source) else {
            return;
        };
        let key = format!("{n} {v} {src}");
        if src.starts_with("git+") {
            lines.insert(short(&key));
        } else {
            let i = usize::from_str_radix(&short(&key)[..2], 16).unwrap_or(0) % 64;
            buckets[i].push(key);
        }
    };
    for package in Package::all(lock) {
        take(Some(package.name), package.version, package.source);
    }
    for (i, mut keys) in buckets.into_iter().enumerate() {
        if !keys.is_empty() {
            keys.sort();
            lines.insert(short(&format!("bucket {i}\n{}", keys.join("\n"))));
        }
    }
    lines.into_iter().collect()
}

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

/// The commit `name` is here, in full.
pub fn commit(dir: &std::path::Path, name: &str) -> Result<String, String> {
    Git(dir)
        .run(&["rev-parse", "--verify", "-q", &format!("{name}^{{commit}}")])
        .map(|s| s.trim().to_string())
        .map_err(|_| format!("no {name} in {}", dir.display()))
}

/// Where `tip` left `from`: the commit an A/B of `from..tip` measures `tip` against. A local branch
/// that is behind its upstream would put that point too early and credit `tip` with commits it
/// merely did not have, so the upstream is asked too and the later of the two answers wins.
/// Returns the commit and, when the upstream decided it, the upstream's name.
pub fn merge_base(
    dir: &std::path::Path,
    from: &str,
    tip: &str,
) -> Result<(String, Option<String>), String> {
    let tip = commit(dir, tip)?;
    let own = commit(dir, from)?;
    let base = |c: &str| {
        Git(dir)
            .run(&["merge-base", c, &tip])
            .map(|s| s.trim().to_string())
    };
    let mine = base(&own)
        .map_err(|_| format!("{from} and {tip:.8} share no history in {}", dir.display()))?;
    let upstream = Git(dir)
        .run(&[
            "rev-parse",
            "--abbrev-ref",
            "-q",
            &format!("{from}@{{upstream}}"),
        ])
        .ok()
        .map(|s| s.trim().to_string());
    let theirs = upstream
        .as_deref()
        .and_then(|u| Some((u.to_string(), base(&commit(dir, u).ok()?).ok()?)));
    match theirs {
        Some((u, b))
            if b != mine
                && Git(dir)
                    .run(&["merge-base", "--is-ancestor", &mine, &b])
                    .is_ok() =>
        {
            Ok((b, Some(u)))
        }
        _ => Ok((mine, None)),
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

#[cfg(test)]
mod tests {

    use super::*;

    /// A repo with an origin, a branch that was never pushed, and a second remote branch whose
    /// tip is what a wrong resolution used to land on. Returns (home, wanted sha, decoy sha).
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

    // A local main behind origin/main puts the base before the branch's real fork point, and the
    // branch is then credited with everything main gained in between.
    #[test]
    fn a_merge_base_is_taken_from_the_upstream_when_the_branch_is_behind_it() {
        let home = std::env::temp_dir().join(format!("dibs-mb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let sh = |cmd: &str| -> String {
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
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        sh("git init -q --bare origin.git && git clone -q origin.git r 2>/dev/null");
        let r = home.join("r");
        let git = |cmd: &str| {
            sh(&format!(
                "cd r && git -c user.email=a@b -c user.name=t {cmd}"
            ))
        };
        git("checkout -q -b main");
        git("commit -q --allow-empty -m c1");
        git("push -q -u origin main");
        let c1 = git("rev-parse HEAD");
        git("commit -q --allow-empty -m c2");
        git("push -q origin main");
        let c2 = git("rev-parse HEAD");
        git("reset -q --hard HEAD~1");
        git("checkout -q -b feat origin/main");
        git("commit -q --allow-empty -m f1");
        assert_eq!(
            merge_base(&r, "main", "feat").unwrap(),
            (c2.clone(), Some("origin/main".to_string()))
        );
        assert_eq!(
            merge_base(&r, "origin/main", "HEAD").unwrap(),
            (c2.clone(), None)
        );
        assert_eq!(
            merge_base(&r, &c1, "feat").unwrap(),
            (c1, None),
            "a commit has no upstream to ask"
        );
        assert!(merge_base(&r, "no-such", "feat").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}

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

pub fn local(dir: &std::path::Path) -> Result<Local, String> {
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
    pub fn local(&self) -> Result<Local, String> {
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
) -> Result<Checkout, String> {
    checkout_in(
        &Paths::from_env()
            .sent()
            .ok_or("no HOME to keep a cache under")?
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
) -> Result<Checkout, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let (slot, lock) = (0u32..)
        .find_map(|n| {
            let path = root.join(format!("{n}.lock"));
            let taken = std::fs::File::create(&path)
                .map_err(std::fs::TryLockError::Error)
                .and_then(|f| f.try_lock().map(|()| f));
            match taken {
                Ok(f) => Some(Ok((n.to_string(), f))),
                Err(std::fs::TryLockError::WouldBlock) => None,
                Err(std::fs::TryLockError::Error(e)) => {
                    Some(Err(format!("{}: {e}", path.display())))
                }
            }
        })
        .expect("an unbounded range")?;
    let checkout = root.join(&slot);
    if !checkout.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&checkout);
        let from = dir
            .to_str()
            .ok_or_else(|| format!("{}: not a path git can take", dir.display()))?;
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

/// The commit `name` means here, the name it was found under, and why it must be sent rather than
/// fetched by name. That is the remote-tracking ref, which a local branch of that name may be
/// behind, unless the local branch has commits origin lacks: fetching it would take origin's.
pub fn as_fetched(
    dir: &std::path::Path,
    name: &str,
) -> Option<(String, String, Option<&'static str>)> {
    let tracking = format!("origin/{name}");
    let Ok(pushed) = commit(dir, &tracking) else {
        return commit(dir, name).ok().map(|c| (c, name.to_string(), None));
    };
    match commit(dir, &format!("refs/heads/{name}")) {
        Ok(own)
            if Git(dir)
                .run(&["merge-base", "--is-ancestor", &own, &pushed])
                .is_err() =>
        {
            Some((own, name.to_string(), Some("origin's branch lacks it")))
        }
        _ => Some((pushed, tracking, None)),
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

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod local_tests {
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

    // Both arms of an A/B shared one target directory, and cargo does not isolate same-name
    // packages by source path, so one arm ran the other's binary.
    #[test]
    fn two_local_trees_never_share_a_cache() {
        let (a, b) = (tmp("a"), tmp("b"));
        repo(&a);
        repo(&b);
        assert_ne!(local(&a).unwrap().key, local(&b).unwrap().key);
    }

    const SIG: &str = "|release||--no-default-features|cpu,fusion|";

    const LOCK_A: &str = "[[package]]\nname = \"cubecl\"\nversion = \"0.11.0\"\nsource = \"git+https://github.com/tracel-ai/cubecl?rev=aaa#aaa\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    fn packages_of(lock: &str) -> String {
        packages(lock, SIG)
            .iter()
            .map(|l| format!("{l}\n"))
            .collect()
    }

    /// A local tree's target with `lock` staged under `token`, as the machine's prepare leaves it.
    fn stage(scratch: &std::path::Path, key: &str, lock: &str, token: &str) {
        let target = scratch.join(format!("target/demo-local-{key}"));
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join(format!(".dibs-packages.pending.{token}")),
            packages_of(lock),
        )
        .unwrap();
    }

    #[test]
    fn a_lockfile_becomes_sorted_hashes_and_a_revision_is_a_different_package() {
        let a = packages_of(LOCK_A);
        assert_eq!(
            a.lines().count(),
            2,
            "one git package, one registry bucket, no workspace member"
        );
        let mut sorted: Vec<&str> = a.lines().collect();
        sorted.sort();
        assert_eq!(sorted, a.lines().collect::<Vec<_>>());
        let b = packages_of(&LOCK_A.replace("rev=aaa#aaa", "rev=bbb#bbb"));
        assert_eq!(a.lines().filter(|l| b.contains(*l)).count(), 1);
        let bumped = packages_of(&LOCK_A.replace("version = \"1.0.0\"", "version = \"1.0.1\""));
        assert_eq!(a.lines().filter(|l| bumped.contains(*l)).count(), 1);
        assert!(packages("", SIG).is_empty());
        let debug = packages(LOCK_A, "|dev||||").join("\n");
        assert_eq!(
            a.lines().filter(|l| debug.contains(*l)).count(),
            0,
            "another profile shares nothing"
        );
    }

    fn age(path: &std::path::Path) -> u64 {
        std::fs::metadata(path)
            .unwrap()
            .modified()
            .unwrap()
            .elapsed()
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// A tree whose one source was written long ago, and the target it builds into.
    fn tree_and_target(
        name: &str,
        claimed_by: Option<&str>,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let scratch = tmp(name);
        let (wt, target) = (scratch.join("ws/demo/abc"), scratch.join("target/demo"));
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(wt.join("src/lib.rs"), "fn f() {}\n").unwrap();
        Command::new("touch")
            .args(["-d", "400 days ago"])
            .arg(wt.join("src/lib.rs"))
            .status()
            .unwrap();
        if let Some(c) = claimed_by {
            let c = if c == "this" {
                wt.canonicalize().unwrap().display().to_string()
            } else {
                c.to_string()
            };
            std::fs::write(target.join(".dibs-tree"), c + "\n").unwrap();
        }
        (scratch, wt, target)
    }

    fn in_tree(wt: &std::path::Path, target: &std::path::Path, script: &str) -> (i32, String) {
        let out = Command::new("bash")
            .arg("-c")
            .arg(script)
            .current_dir(wt)
            .env("CARGO_TARGET_DIR", target)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    }

    #[test]
    fn a_build_after_another_tree_s_dates_this_tree_s_sources_after_it() {
        let (scratch, wt, target) = tree_and_target("claim-other", Some("/another/tree"));
        let (code, out) = in_tree(&wt, &target, &claiming("echo BUILT"));
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains("BUILT") && out.contains("did not make the last build"),
            "{out}"
        );
        assert!(age(&wt.join("src/lib.rs")) < 3600);
        let claimed = std::fs::read_to_string(target.join(".dibs-tree")).unwrap();
        assert_eq!(
            claimed.trim(),
            wt.canonicalize().unwrap().display().to_string()
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // A rerun of one tree compiles nothing and is right to, so nothing is redated for it.
    #[test]
    fn a_build_after_the_same_tree_s_leaves_its_sources_alone() {
        let (scratch, wt, target) = tree_and_target("claim-same", Some("this"));
        let (code, out) = in_tree(&wt, &target, &claiming("echo BUILT"));
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("did not make the last build"), "{out}");
        assert!(age(&wt.join("src/lib.rs")) > 86400 * 300);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_measurement_runs_only_where_its_own_tree_made_the_last_build() {
        for (claimed_by, want) in [(Some("this"), 0), (Some("/another/tree"), 78), (None, 78)] {
            let (scratch, wt, target) = tree_and_target("check", claimed_by);
            let (code, out) = in_tree(&wt, &target, &checked("echo MEASURED"));
            assert_eq!(code, want, "{claimed_by:?}: {out}");
            assert_eq!(out.contains("MEASURED"), want == 0, "{claimed_by:?}: {out}");
            assert_eq!(
                out.lines().any(|l| l == "DIBS-REFUSED"),
                want == 78,
                "{claimed_by:?}: {out}"
            );
            let _ = std::fs::remove_dir_all(&scratch);
        }
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

    /// Runs a step through `recording`, without the `cd` and export dibs puts around it.
    fn step(scratch: &std::path::Path, key: &str, token: &str, run: &str) {
        let target = scratch.join(format!("target/demo-local-{key}"));
        let script = format!(
            "export CARGO_TARGET_DIR={}\n{}",
            target.display(),
            recording(run, token)
        );
        Command::new("bash").args(["-c", &script]).status().unwrap();
    }

    fn record(scratch: &std::path::Path, key: &str) -> Option<String> {
        std::fs::read_to_string(scratch.join(format!("target/demo-local-{key}/.dibs-packages")))
            .ok()
    }

    #[test]
    fn a_target_is_credited_with_a_lockfile_only_once_a_build_succeeds() {
        let scratch = tmp("record-success");
        stage(&scratch, "k", LOCK_A, "t1");
        assert_eq!(
            record(&scratch, "k"),
            None,
            "a prepare alone records nothing"
        );
        step(&scratch, "k", "t1", "false");
        assert_eq!(
            record(&scratch, "k"),
            None,
            "a failed build records nothing"
        );
        step(&scratch, "k", "t1", "true; exit 0");
        assert_eq!(
            record(&scratch, "k").unwrap().lines().count(),
            2,
            "a command that exits itself still records"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_wrapped_step_keeps_the_commands_exit_status() {
        let scratch = tmp("record-exit");
        std::fs::create_dir_all(scratch.join("target/demo-local-k")).unwrap();
        let script = format!(
            "export CARGO_TARGET_DIR={}\n{}",
            scratch.join("target/demo-local-k").display(),
            recording("exit 7", "t1")
        );
        assert_eq!(
            Command::new("bash")
                .args(["-c", &script])
                .status()
                .unwrap()
                .code(),
            Some(7)
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // Two agents prepare one target and the first one's build finishes after the second prepare.
    #[test]
    fn a_build_merges_only_what_its_own_prepare_staged() {
        let scratch = tmp("record-own");
        stage(&scratch, "k", LOCK_A, "t1");
        stage(
            &scratch,
            "k",
            &LOCK_A.replace("rev=aaa#aaa", "rev=bbb#bbb"),
            "t2",
        );
        step(&scratch, "k", "t1", "true");
        assert_eq!(record(&scratch, "k").unwrap(), packages_of(LOCK_A));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // The union, not the last lockfile: artifacts from an earlier revision are still there.
    #[test]
    fn a_target_remembers_every_lockfile_built_into_it() {
        let scratch = tmp("record");
        stage(&scratch, "k", LOCK_A, "t1");
        step(&scratch, "k", "t1", "true");
        stage(
            &scratch,
            "k",
            &LOCK_A.replace("rev=aaa#aaa", "rev=bbb#bbb"),
            "t2",
        );
        step(&scratch, "k", "t2", "true");
        assert_eq!(record(&scratch, "k").unwrap().lines().count(), 3);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_signature_is_what_a_build_depends_on_besides_the_lockfile() {
        let a = build_signature("start=$(date +%s); cargo build --release -p app --no-default-features --features cpu,fusion; rc=$?").unwrap();
        let b = build_signature(
            "cargo build -p app --features fusion,cpu --release --no-default-features",
        )
        .unwrap();
        assert_eq!(a, b, "flag order and feature order do not matter");
        assert_eq!(a, "|release||--no-default-features|cpu,fusion|");
        assert_ne!(
            a,
            build_signature("cargo build -p app --no-default-features --features cpu,fusion")
                .unwrap()
        );
        assert_eq!(
            build_signature("cargo bench --no-run").unwrap(),
            "|release||||"
        );
        assert_eq!(
            build_signature("cargo +nightly bench").unwrap(),
            "nightly|release||||"
        );
        assert_eq!(
            build_signature("cargo test --profile=ci -Fa --target x86_64-unknown-linux-gnu")
                .unwrap(),
            "|ci|x86_64-unknown-linux-gnu||a|"
        );
        assert_eq!(
            build_signature("RUSTFLAGS=-Ctarget-cpu=native ~/.cargo/bin/cargo build").unwrap(),
            "|dev||||-Ctarget-cpu=native"
        );
        assert_eq!(
            build_signature("cargo build && ./target/debug/app --features x --release").unwrap(),
            "|dev||||",
            "flags of a later command are not cargo's"
        );
        assert_eq!(
            build_signature("cargo run --release -- --features x").unwrap(),
            "|release||||",
            "arguments after -- go to the program"
        );
    }

    #[test]
    fn commands_that_leave_no_usable_artifacts_record_nothing() {
        for run in [
            "cargo fmt --check",
            "cargo clippy --release",
            "cargo check",
            "cargo --version",
            "./target/release/app bench",
        ] {
            assert_eq!(build_signature(run), None, "{run}");
        }
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
