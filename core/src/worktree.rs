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

use std::fmt::Write as _;

/// Collection runs on every prepare, fetched or local, and sweeps every repo's trees and targets
/// rather than only the one being prepared: most runs are local, and a repo nobody prepares any
/// more would otherwise never be swept at all.
///
/// A target directory goes on a short clock because the disk is what runs out first on a
/// machine, and a compilation cache makes refilling one cheap. One with no marker predates the
/// marker, so it is dated rather than deleted. The current tree and target were touched a moment
/// ago, and a running job touched its own when it started, so neither can be a victim.
const GC: &str = r#"KEEP=${DIBS_KEEP_DAYS:-14}
for old in "$SCRATCH"/ws/*/*; do
    [ -d "$old" ] || continue
    [ "$old" = "$WT" ] && continue
    [ -n "$(find "$old/.dibs-used" -maxdepth 0 -mtime +"$KEEP" 2>/dev/null)" ] || continue
    echo "DIBS-GC $old" >&2
    git -C "$old" worktree remove --force "$old" 2>/dev/null || rm -rf "$old"
done
TKEEP=${DIBS_TARGET_KEEP_DAYS:-5}
for old in "$SCRATCH/target"/*; do
    [ -d "$old" ] || continue
    [ "$old" = "$TARGET" ] && continue
    if [ ! -e "$old/.dibs-used" ]; then touch "$old/.dibs-used"; continue; fi
    [ -n "$(find "$old/.dibs-used" -maxdepth 0 -mtime +"$TKEEP" 2>/dev/null)" ] || continue
    echo "DIBS-GC $old ($(du -sh "$old" 2>/dev/null | cut -f1))" >&2
    rm -rf "$old"
done
"#;

/// A new local tree starts from a copy of its repo's most recently used target directory rather
/// than from nothing, since trees of one repo differ in a few files. Only a tree that is itself
/// new: the sync that follows gives every file the current time, which is what makes cargo
/// rebuild the workspace crates instead of trusting the copy. A tree that outlived its target
/// keeps old times, so a copy there could be taken as fresh.
///
/// A sibling a build holds is skipped, since cargo writes artifacts before their fingerprints.
/// Only a reflink copy is made: a full one per tree would fill the disk, and a filesystem that
/// cannot share blocks gets no seed at all.
const SEED: &str = r#"for used in $(ls -t "$SCRATCH/target/{repo}/.dibs-used" "$SCRATCH/target/{repo}"-local-*/.dibs-used 2>/dev/null); do
    src=${used%/.dibs-used}
    fds=
    held=0
    for lock in $(find "$src" -maxdepth 3 -name .cargo-lock 2>/dev/null); do
        exec {fd}<"$lock"
        fds="$fds $fd"
        flock -n -s "$fd" || { held=1; break; }
    done
    if [ "$held" = 0 ] && cp -a --reflink=always "$src" "$TARGET.seed.$$" 2>/dev/null && mv -T "$TARGET.seed.$$" "$TARGET" 2>/dev/null; then
        echo "DIBS-SEED $src" >&2
    fi
    for fd in $fds; do exec {fd}<&-; done
    rm -rf "$TARGET.seed.$$"
    [ -d "$TARGET" ] && break
done
"#;

/// Where everything lives, under the account's scratch. Keyed by commit rather than by branch
/// name: two agents on the same branch at different commits then get different trees instead
/// of racing to check out over each other, and a rerun of the same commit reuses its tree.
///
/// The cost is that trees accumulate, which is a garbage collection problem rather than a
/// correctness one, and dibs can solve it precisely because it owns the layout: it knows when
/// each tree was last used, which nobody could know when the paths were hand-written.
pub fn setup_script(repo: &str, reference: &str) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        r#"set -eu
SCRATCH=${{DIBS_SCRATCH:-${{DIBS_SCRATCH:-$HOME/.cache/dibs}}}}
SRC=$HOME/prog/{repo}
[ -d "$SRC/.git" ] || {{ echo "dibs: no clone at $SRC" >&2; exit 3; }}

# Fetch before resolving, or a ref that exists only on the remote cannot be found. Quiet
# because a fetch's progress is noise in a job's output, but not silent on failure.
#
# Fetched into a ref of this job's own, never through FETCH_HEAD. There is one FETCH_HEAD per
# repository and every job here shares one clone, so a second prepare fetching a different
# branch between this one's fetch and its read hands it that branch instead. It resolves, it
# looks right, and the number it produces is for code nobody asked about. Prepares run under
# the shared lock precisely so several can happen at once, which makes that race the normal
# case rather than a rare one.
MINE=refs/dibs/prepare-$$
if FETCHERR=$(git -C "$SRC" fetch -q origin "+{reference}:$MINE" 2>&1); then
    SHA=$(git -C "$SRC" rev-parse --verify -q "$MINE^{{commit}}" || true)
    git -C "$SRC" update-ref -d "$MINE" 2>/dev/null || true
else
    # A bare commit cannot be fetched by name from most servers, and a branch that exists only
    # on this machine cannot be fetched at all. Both resolve locally, by their own name, which
    # is not a shared slot and cannot be overwritten by anyone else.
    FETCHERR="$FETCHERR
$(git -C "$SRC" fetch -q --all 2>&1 || true)"
    SHA=$(git -C "$SRC" rev-parse --verify -q '{reference}^{{commit}}' || true)
fi
# A fetch that failed on credentials means this machine cannot see the remote at all, and the
# ref it was asked for is very likely fine. Reported as "no such ref" it reads as a mistake in
# the ref, and the way out of that reading is to carry the code over by hand, which is a whole
# afternoon of bundle or tarball for something @local already does.
[ -n "$SHA" ] || {{
    echo "dibs: no such ref in {repo}: {reference}" >&2
    case "$FETCHERR" in
      *"could not read Username"*|*"Authentication failed"*|*"terminal prompts disabled"*|\
      *"Permission denied (publickey)"*|*"Repository not found"*)
        echo "  The fetch failed on credentials, so nothing here can see that remote: a private" >&2
        echo "  repo is the usual reason, and the ref itself is probably fine." >&2
        echo "  Send your working tree instead, which fetches nothing:  {repo}@local" >&2 ;;
    esac
    exit 3; }}
SHORT=$(printf %s "$SHA" | cut -c1-12)

WT=$SCRATCH/ws/{repo}/$SHORT
mkdir -p "$SCRATCH/ws/{repo}"
# Two prepares of one commit both see no tree and both add it, and the loser dies on "already
# exists". Per repo rather than per commit, because the prune below touches every worktree.
exec 7>"$SCRATCH/ws/{repo}/.prepare.lock"
flock 7
if [ ! -d "$WT/.git" ] && [ ! -f "$WT/.git" ]; then
    # Detached on purpose: a worktree that tracks a branch would move under a job that is
    # still measuring from it.
    git -C "$SRC" worktree add --detach -q "$WT" "$SHA" 2>/dev/null || {{
        # A tree left behind by a crash is registered but absent; prune and retry once.
        git -C "$SRC" worktree prune
        git -C "$SRC" worktree add --detach -q "$WT" "$SHA"; }}
fi
exec 7>&-
touch "$WT/.dibs-used"

# One cache per repo rather than per tree. Cargo fingerprints per crate, so switching commits
# reuses most of it, where a tree of its own would rebuild the world every commit. Concurrent
# builds serialise on cargo's own lock, which is correct.
TARGET=$SCRATCH/target/{repo}
mkdir -p "$TARGET" "$SCRATCH/out"
touch "$TARGET/.dibs-used"

{gc}git -C "$SRC" worktree prune

echo "DIBS-WT $WT"
echo "DIBS-TARGET $TARGET"
echo "DIBS-REV {repo} $SHORT"
"#,
        gc = GC
    );
    s
}

pub struct Prepared {
    pub worktree: String,
    pub target: String,
    pub revisions: Vec<(String, String)>,
}

/// Reads the markers back out. Anything else the setup printed is left alone, so a fetch that
/// says something useful still reaches the caller.
pub fn parse(out: &str) -> Result<Prepared, String> {
    let mut worktree = None;
    let mut target = None;
    let mut revisions = Vec::new();
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("DIBS-WT ") {
            worktree = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("DIBS-TARGET ") {
            target = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("DIBS-REV ") {
            let mut it = v.split_whitespace();
            if let (Some(r), Some(sha)) = (it.next(), it.next()) {
                revisions.push((r.to_string(), sha.to_string()));
            }
        }
    }
    match (worktree, target) {
        (Some(worktree), Some(target)) => Ok(Prepared { worktree, target, revisions }),
        _ => Err("the worktree setup did not report a path; see its output above".into()),
    }
}

#[cfg(test)]
mod tests {

    // The setup script is shell living inside a Rust format string, where an unbalanced quote
    // or a brace that needed doubling compiles cleanly and fails on the machine, mid-run,
    // holding a lock.
    #[test]
    fn the_generated_scripts_are_valid_shell() {
        use std::io::Write;
        for script in [setup_script("cubek", "main"), setup_local_script("cubek", "k1", "c1")] {
            let mut c = std::process::Command::new("bash")
                .arg("-n")
                .stdin(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("bash on PATH");
            c.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
            let out = c.wait_with_output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    }

    // A machine with no credentials for a private remote reported "no such ref", which reads as
    // the ref being wrong and sends people to carry the code over by hand.
    #[test]
    fn a_credentials_failure_names_local_rather_than_the_ref() {
        let s = setup_script("cubek", "main");
        assert!(s.contains("could not read Username"), "the fetch error has to be inspected");
        assert!(s.contains("cubek@local"), "and the way out has to be named");
    }
    use super::*;

    #[test]
    fn markers_are_read_back_and_other_output_ignored() {
        let out = "Fetching origin\nDIBS-WT /s/ws/cubek/abc123def456\n\
                   DIBS-TARGET /s/target/cubek\nDIBS-SCRATCH /s\nDIBS-REV cubek abc123def456\n";
        let p = parse(out).unwrap();
        assert_eq!(p.worktree, "/s/ws/cubek/abc123def456");
        assert_eq!(p.target, "/s/target/cubek");
        assert_eq!(p.revisions, vec![("cubek".to_string(), "abc123def456".to_string())]);
    }

    #[test]
    fn a_setup_that_printed_nothing_useful_is_an_error_not_an_empty_path() {
        assert!(parse("fatal: not a git repository\n").is_err());
    }

    #[test]
    fn collection_can_never_remove_the_tree_just_prepared() {
        let s = setup_script("cubek", "main");
        assert!(s.contains(r#"[ "$old" = "$WT" ] && continue"#),
                "the current tree must be excluded from collection by identity, not by luck");
        assert!(s.contains("-mtime +"), "collection has to be by age, not unconditional");
    }

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
            let o = std::process::Command::new("bash").arg("-c").arg(cmd).current_dir(&home).output().unwrap();
            assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
        };
        sh("git -C cubek init -q && git -C cubek -c user.email=a@b -c user.name=t commit -q --allow-empty -m one");
        sh("git -C cubek worktree add -q ../topk-branch");
        assert_eq!(identity(&home.join("topk-branch")), "cubek");
        assert_eq!(identity(&main), "cubek");
        assert_eq!(identity(&main.join("inner")), "inner");
        assert_eq!(identity(&home.join("loose")), "loose");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn sandbox(name: &str) -> (std::path::PathBuf, String, String) {
        let home = std::env::temp_dir().join(format!("dibs-wt-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let src = home.join("prog/demo");
        std::fs::create_dir_all(&src).unwrap();
        let sh = |dir: &std::path::Path, cmd: &str| -> String {
            let o = std::process::Command::new("bash")
                .arg("-c").arg(cmd).current_dir(dir).output().unwrap();
            assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        sh(&home, "git init -q --bare origin.git");
        sh(&home, "git clone -q origin.git prog/demo");
        sh(&src, "git config user.email a@b && git config user.name t");
        sh(&src, "echo one > f && git add -A && git commit -qm first && git push -q origin HEAD:main");
        sh(&src, "git checkout -q -b decoy && echo decoy > f && git commit -qam decoy && git push -q origin decoy");
        let decoy = sh(&src, "git rev-parse HEAD");
        sh(&src, "git checkout -q -B local-only origin/main && echo real > f && git commit -qam real");
        let wanted = sh(&src, "git rev-parse HEAD");
        (home, wanted, decoy)
    }

    fn resolve(home: &std::path::Path, reference: &str) -> (bool, String) {
        let out = std::process::Command::new("bash")
            .arg("-c").arg(setup_script("demo", reference))
            .env("HOME", home)
            .env("DIBS_SCRATCH", home.join("scratch"))
            .output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let sha = text.lines().find_map(|l| l.strip_prefix("DIBS-REV demo "))
            .unwrap_or("").trim().to_string();
        (out.status.success(), sha)
    }

    // A branch that exists only locally cannot be fetched, and the fallback fetch writes
    // FETCH_HEAD with something else entirely. Reading it there resolved every such ref to one
    // unrelated commit and built it without complaint.
    #[test]
    fn a_ref_that_cannot_be_fetched_still_resolves_to_itself() {
        let (home, wanted, decoy) = sandbox("local");
        let (ok, sha) = resolve(&home, "local-only");
        assert!(ok, "preparing a local-only branch should succeed");
        assert_eq!(sha, wanted[..12], "resolved to something other than the ref asked for");
        assert_ne!(sha, decoy[..12]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn concurrent_prepares_of_one_commit_all_succeed() {
        let (home, wanted, _) = sandbox("concurrent");
        let kids: Vec<_> = (0..8)
            .map(|_| {
                std::process::Command::new("bash")
                    .arg("-c").arg(setup_script("demo", &wanted))
                    .env("HOME", &home)
                    .env("DIBS_SCRATCH", home.join("scratch"))
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn().unwrap()
            })
            .collect();
        for k in kids {
            let out = k.wait_with_output().unwrap();
            assert!(out.status.success(), "a prepare failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    // Reported from a real run: `dibs test cubecl@perf/fma-fusion-backends cuda` built
    // something else, said nothing, and returned a believable number.
    #[test]
    fn a_branch_name_containing_a_slash_resolves_to_itself() {
        let (home, _, decoy) = sandbox("slash");
        let sh = |cmd: &str| {
            let o = std::process::Command::new("bash")
                .arg("-c").arg(cmd).current_dir(home.join("prog/demo")).output().unwrap();
            assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        sh("git checkout -q -b perf/some-work && echo slashed > f && git commit -qam slashed");
        let wanted = sh("git rev-parse HEAD");
        sh("git push -q origin perf/some-work");
        let (ok, sha) = resolve(&home, "perf/some-work");
        assert!(ok, "a branch with a slash should prepare");
        assert_eq!(sha, wanted[..12], "resolved somewhere else entirely");
        assert_ne!(sha, decoy[..12]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn two_refs_do_not_resolve_to_the_same_commit() {
        let (home, wanted, _) = sandbox("two");
        let (_, a) = resolve(&home, "local-only");
        let (_, b) = resolve(&home, "main");
        assert_eq!(a, wanted[..12]);
        assert_ne!(a, b, "different refs must not land in one workspace");
        let _ = std::fs::remove_dir_all(&home);
    }

    // The failure this makes possible is the quiet one: a typo that builds whatever FETCH_HEAD
    // happened to hold, and reports errors from code the caller never named.
    #[test]
    fn a_ref_that_does_not_exist_is_refused() {
        let (home, _, _) = sandbox("missing");
        let (ok, sha) = resolve(&home, "no-such-branch");
        assert!(!ok, "an unknown ref must fail, not resolve to something else");
        assert!(sha.is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Three target directories: the one about to be used, one last touched long ago, and one
    /// that has never been marked at all because it predates the marker.
    fn targets(home: &std::path::Path, keep: &str, script: &str) -> Vec<String> {
        let t = home.join("scratch/target");
        std::fs::create_dir_all(t.join("abandoned")).unwrap();
        std::fs::create_dir_all(t.join("unmarked")).unwrap();
        std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "touch -d '400 days ago' {}/abandoned/.dibs-used",
                t.display()
            ))
            .status()
            .unwrap();
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("HOME", home)
            .env("DIBS_SCRATCH", home.join("scratch"))
            .env("DIBS_TARGET_KEEP_DAYS", keep)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let mut left: Vec<String> = std::fs::read_dir(&t)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        left.sort();
        left
    }

    // The point of collecting at all: a repo nobody builds here any more keeps a target
    // directory forever, and it is the largest thing dibs puts on a machine.
    #[test]
    fn a_target_directory_nobody_has_used_is_collected() {
        let (home, _, _) = sandbox("gc");
        let left = targets(&home, "45", &setup_script("demo", "local-only"));
        assert!(!left.contains(&"abandoned".to_string()), "left {left:?}");
        assert!(left.contains(&"demo".to_string()), "the one being used must survive");
        let _ = std::fs::remove_dir_all(&home);
    }

    // Most runs are local, so a sweep that only a fetched ref triggers never runs at all.
    #[test]
    fn a_local_prepare_collects_stale_targets_and_trees_of_every_repo() {
        let (home, _, _) = sandbox("gc-local");
        let stale = home.join("scratch/ws/other/local-old");
        std::fs::create_dir_all(&stale).unwrap();
        std::process::Command::new("touch")
            .arg("-d")
            .arg("400 days ago")
            .arg(stale.join(".dibs-used"))
            .status()
            .unwrap();
        let left = targets(&home, "5", &setup_local_script("demo", "k", "c"));
        assert!(!left.contains(&"abandoned".to_string()), "left {left:?}");
        assert!(left.contains(&"demo-local-k".to_string()), "the one being used must survive");
        assert!(!stale.exists(), "a stale tree of another repo must be collected");
        let _ = std::fs::remove_dir_all(&home);
    }

    // Everything already on a machine predates the marker. Treating that as "never used"
    // would delete every target directory on the first run after an upgrade.
    #[test]
    fn a_target_directory_with_no_marker_is_dated_rather_than_deleted() {
        let (home, _, _) = sandbox("unmarked");
        let left = targets(&home, "45", &setup_script("demo", "local-only"));
        assert!(left.contains(&"unmarked".to_string()), "left {left:?}");
        assert!(home.join("scratch/target/unmarked/.dibs-used").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    // One FETCH_HEAD per repository, one clone per machine, and prepares that run concurrently
    // by design. Reading it is a race whose losing outcome is a believable wrong answer, so the
    // rule is structural: this script must never consult it.
    #[test]
    fn resolution_never_goes_through_the_shared_fetch_head() {
        let s = setup_script("cubek", "main");
        // Comments may name it; the code may not use it.
        let code: String = s
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!code.contains("FETCH_HEAD"), "FETCH_HEAD is shared by every job on the machine");
        assert!(code.contains("refs/dibs/prepare-$$"), "each prepare needs a ref of its own");
    }

    #[test]
    fn the_script_keys_the_tree_by_commit_not_by_ref() {
        let s = setup_script("cubek", "main");
        assert!(s.contains("ws/cubek/$SHORT"), "tree path must be keyed by the resolved commit");
        assert!(s.contains("--detach"), "a tracking worktree would move under a running job");
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
    let top = git(dir, &["rev-parse", "--show-toplevel"]).map(|t| std::path::PathBuf::from(t.trim()));
    if top.ok().and_then(|t| t.canonicalize().ok()) != dir.canonicalize().ok() {
        return fallback;
    }
    let Ok(common) = git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"]) else {
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

fn git(dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn local(dir: &std::path::Path) -> Result<Local, String> {
    use sha2::{Digest, Sha256};
    let head = git(dir, &["rev-parse", "--short", "HEAD"])?.trim().to_string();
    // Tracked and untracked-but-not-ignored, which is the same set the sync carries, so the
    // hash describes what was actually built rather than what was committed.
    let list = git(dir, &["ls-files", "-co", "--exclude-standard", "-z"])?;
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
    if !git(dir, &["status", "--porcelain"])?.trim().is_empty() {
        dirty = true;
    }
    let content = format!("{head}{}-{:.12}", if dirty { "+dirty" } else { "" }, hex(&h.finalize()));
    let mut k = Sha256::new();
    k.update(dir.as_os_str().as_encoded_bytes());
    Ok(Local { key: format!("{:.10}", hex(&k.finalize())), content, dirty })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The same layout a fetched ref gets, without the fetch: the tree arrives by rsync instead.
/// Everything downstream, the lock split, the recorded revision and the log path, is unchanged,
/// which is the point. Re-implementing that by hand is what produced two wrong numbers.
pub fn setup_local_script(repo: &str, key: &str, content: &str) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        r#"set -eu
SCRATCH=${{DIBS_SCRATCH:-$HOME/.cache/dibs}}
WT=$SCRATCH/ws/{repo}/local-{key}
# Its own target directory, so two local trees of one repo cannot hand each other a binary.
TARGET=$SCRATCH/target/{repo}-local-{key}
if [ ! -d "$WT" ] && [ ! -d "$TARGET" ]; then
{seed}fi
mkdir -p "$WT" "$TARGET" "$SCRATCH/out"
touch "$WT/.dibs-used" "$TARGET/.dibs-used"
{gc}echo "DIBS-WT $WT"
echo "DIBS-TARGET $TARGET"
echo "DIBS-REV {repo} local:{content}"
"#,
        gc = GC,
        seed = SEED.replace("{repo}", repo)
    );
    s
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
            Command::new("git").arg("-C").arg(dir).args(a).status().unwrap();
        }
        std::fs::write(dir.join("a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "target\n").unwrap();
        Command::new("git").arg("-C").arg(dir).args(["add", "-A"]).status().unwrap();
        Command::new("git").arg("-C").arg(dir).args(["commit", "-qm", "one"]).status().unwrap();
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
        assert!(setup_local_script("r", &local(&a).unwrap().key, "x")
            .contains("target/r-local-"));
    }

    /// `cp` stands in for the filesystem: the test directory is usually a tmpfs, which has no
    /// reflinks, so a shim decides whether the reflink copy succeeds.
    fn prepare_local(scratch: &std::path::Path, key: &str, hold: Option<&str>, cp: &str) -> String {
        let script = setup_local_script("demo", key, "c");
        let bin = scratch.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let real = String::from_utf8(Command::new("bash").args(["-c", "type -P cp"]).output().unwrap().stdout).unwrap();
        let shim = match cp {
            "reflinks" => format!("#!/bin/bash\nargs=()\nshared=0\nfor a; do if [ \"$a\" = --reflink=always ]; then shared=1; else args+=(\"$a\"); fi; done\n[ $shared = 1 ] || exit 1\nexec {} \"${{args[@]}}\"\n", real.trim()),
            _ => "#!/bin/bash\nexit 1\n".to_string(),
        };
        std::fs::write(bin.join("cp"), shim).unwrap();
        Command::new("chmod").arg("+x").arg(bin.join("cp")).status().unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        let mut cmd = Command::new("bash");
        cmd.env("PATH", path);
        match hold {
            Some(lock) => cmd.args(["-c", &format!("flock -x {lock} bash -c \"$0\"")]).arg(&script),
            None => cmd.arg("-c").arg(&script),
        };
        let out = cmd.env("DIBS_SCRATCH", scratch).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn sibling(scratch: &std::path::Path) -> std::path::PathBuf {
        let t = scratch.join("target/demo-local-old");
        std::fs::create_dir_all(t.join("debug/deps")).unwrap();
        std::fs::write(t.join("debug/deps/libdep.rlib"), "artifact\n").unwrap();
        std::fs::write(t.join("debug/.cargo-lock"), "").unwrap();
        std::fs::write(t.join(".dibs-used"), "").unwrap();
        t
    }

    #[test]
    fn a_new_tree_starts_from_its_repos_latest_target() {
        let scratch = tmp("seed");
        sibling(&scratch);
        let err = prepare_local(&scratch, "new", None, "reflinks");
        assert!(err.contains("DIBS-SEED"), "{err}");
        assert!(scratch.join("target/demo-local-new/debug/deps/libdep.rlib").exists());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_target_a_build_holds_is_not_copied() {
        let scratch = tmp("seed-held");
        let old = sibling(&scratch);
        prepare_local(&scratch, "new", Some(old.join("debug/.cargo-lock").to_str().unwrap()), "reflinks");
        assert!(!scratch.join("target/demo-local-new/debug").exists());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_filesystem_without_reflinks_gets_no_seed_and_no_partial_copy() {
        let scratch = tmp("seed-no-reflink");
        sibling(&scratch);
        prepare_local(&scratch, "new", None, "no reflinks");
        assert!(!scratch.join("target/demo-local-new/debug").exists());
        let leftovers = std::fs::read_dir(scratch.join("target")).unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".seed.")).count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // Its files keep the times of an earlier sync, so cargo could take a copied artifact as fresh.
    #[test]
    fn a_tree_that_outlived_its_target_is_not_seeded() {
        let scratch = tmp("seed-old-tree");
        sibling(&scratch);
        std::fs::create_dir_all(scratch.join("ws/demo/local-new")).unwrap();
        prepare_local(&scratch, "new", None, "reflinks");
        assert!(!scratch.join("target/demo-local-new/debug").exists());
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
