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
if git -C "$SRC" fetch -q origin "+{reference}:$MINE" 2>/dev/null; then
    SHA=$(git -C "$SRC" rev-parse --verify -q "$MINE^{{commit}}" || true)
    git -C "$SRC" update-ref -d "$MINE" 2>/dev/null || true
else
    # A bare commit cannot be fetched by name from most servers, and a branch that exists only
    # on this machine cannot be fetched at all. Both resolve locally, by their own name, which
    # is not a shared slot and cannot be overwritten by anyone else.
    git -C "$SRC" fetch -q --all 2>/dev/null || true
    SHA=$(git -C "$SRC" rev-parse --verify -q '{reference}^{{commit}}' || true)
fi
[ -n "$SHA" ] || {{ echo "dibs: no such ref in {repo}: {reference}" >&2; exit 3; }}
SHORT=$(printf %s "$SHA" | cut -c1-12)

WT=$SCRATCH/ws/{repo}/$SHORT
if [ ! -d "$WT/.git" ] && [ ! -f "$WT/.git" ]; then
    mkdir -p "$SCRATCH/ws/{repo}"
    # Detached on purpose: a worktree that tracks a branch would move under a job that is
    # still measuring from it.
    git -C "$SRC" worktree add --detach -q "$WT" "$SHA" 2>/dev/null || {{
        # A tree left behind by a crash is registered but absent; prune and retry once.
        git -C "$SRC" worktree prune
        git -C "$SRC" worktree add --detach -q "$WT" "$SHA"; }}
fi
touch "$WT/.dibs-used"

# One cache per repo rather than per tree. Cargo fingerprints per crate, so switching commits
# reuses most of it, where a tree of its own would rebuild the world every commit. Concurrent
# builds serialise on cargo's own lock, which is correct and is the trade being made until
# there is a shared sccache.
TARGET=$SCRATCH/target/{repo}
mkdir -p "$TARGET" "$SCRATCH/out"
touch "$TARGET/.dibs-used"

# Trees are keyed by commit, so they accumulate: every commit ever measured leaves one. That
# is a garbage collection problem rather than a correctness one, and it is solvable here for
# exactly the reason the layout is owned at all, which is that the last use of each tree is
# known. Nobody could know that when the paths were written by hand.
#
# The current tree was touched a moment ago, so it can never be its own victim, and a job that
# is still running touched its tree when it started.
KEEP=${{DIBS_KEEP_DAYS:-14}}
for old in "$SCRATCH/ws/{repo}"/*; do
    [ -d "$old" ] || continue
    [ "$old" = "$WT" ] && continue
    [ -n "$(find "$old/.dibs-used" -maxdepth 0 -mtime +"$KEEP" 2>/dev/null)" ] || continue
    echo "DIBS-GC $old" >&2
    git -C "$SRC" worktree remove --force "$old" 2>/dev/null || rm -rf "$old"
done
git -C "$SRC" worktree prune

# Target directories are not worktrees. There is one per repo, every tree of that repo shares
# it, and it is the thing that makes a build fast rather than a by-product of one, so it is
# collected only when a repo has stopped being built here at all and on a much longer clock. A
# compilation cache on the machine is what makes this reasonable at all: refilling a collected
# directory costs a fraction of what filling it the first time did.
#
# A directory with no marker gets one rather than being removed. Everything already on disk
# predates the marker, and starting its clock is the answer that cannot delete something that
# is still in daily use.
TKEEP=${{DIBS_TARGET_KEEP_DAYS:-45}}
for old in "$SCRATCH/target"/*; do
    [ -d "$old" ] || continue
    [ "$old" = "$TARGET" ] && continue
    if [ ! -e "$old/.dibs-used" ]; then touch "$old/.dibs-used"; continue; fi
    [ -n "$(find "$old/.dibs-used" -maxdepth 0 -mtime +"$TKEEP" 2>/dev/null)" ] || continue
    echo "DIBS-GC $old ($(du -sh "$old" 2>/dev/null | cut -f1))" >&2
    rm -rf "$old"
done

echo "DIBS-WT $WT"
echo "DIBS-TARGET $TARGET"
echo "DIBS-SCRATCH $SCRATCH"
echo "DIBS-REV {repo} $SHORT"
"#
    );
    s
}

pub struct Prepared {
    pub worktree: String,
    pub target: String,
    pub scratch: String,
    pub revisions: Vec<(String, String)>,
}

/// Reads the markers back out. Anything else the setup printed is left alone, so a fetch that
/// says something useful still reaches the caller.
pub fn parse(out: &str) -> Result<Prepared, String> {
    let mut worktree = None;
    let mut target = None;
    let mut scratch = None;
    let mut revisions = Vec::new();
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("DIBS-WT ") {
            worktree = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("DIBS-TARGET ") {
            target = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("DIBS-SCRATCH ") {
            scratch = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("DIBS-REV ") {
            let mut it = v.split_whitespace();
            if let (Some(r), Some(sha)) = (it.next(), it.next()) {
                revisions.push((r.to_string(), sha.to_string()));
            }
        }
    }
    match (worktree, target, scratch) {
        (Some(worktree), Some(target), Some(scratch)) => {
            Ok(Prepared { worktree, target, scratch, revisions })
        }
        _ => Err("the worktree setup did not report a path; see its output above".into()),
    }
}

#[cfg(test)]
mod tests {
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

    // Reported from a real run: `dibs-run test cubecl@perf/fma-fusion-backends cuda` built
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
    fn targets(home: &std::path::Path, keep: &str) -> Vec<String> {
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
            .arg(setup_script("demo", "local-only"))
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
        let left = targets(&home, "45");
        assert!(!left.contains(&"abandoned".to_string()), "left {left:?}");
        assert!(left.contains(&"demo".to_string()), "the one being used must survive");
        let _ = std::fs::remove_dir_all(&home);
    }

    // Everything already on a machine predates the marker. Treating that as "never used"
    // would delete every target directory on the first run after an upgrade.
    #[test]
    fn a_target_directory_with_no_marker_is_dated_rather_than_deleted() {
        let (home, _, _) = sandbox("unmarked");
        let left = targets(&home, "45");
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
