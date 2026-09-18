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
    if [ ! -e "$old/.dibs-used" ]; then touch "$old/.dibs-used"; continue; fi
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

/// A new local tree starts from a reflink copy of a sibling's target directory, and of that
/// sibling's sources when it has some, since trees of one repo differ in a few files. The sibling
/// is the one that has built the most of this tree's `Cargo.lock`, newest first among equals:
/// cargo reuses a crate only at the same version and git revision, so the newest sibling on
/// another revision saves little.
///
/// The sync that follows rewrites only files whose bytes differ, and a rewritten file takes the
/// current time, so cargo rebuilds the crates whose sources changed and trusts the rest. Sources
/// and target come from one sibling under one set of locks, because a file dated from one tree
/// and compared against another tree's artifacts could pass for fresh. Only a tree that is itself
/// new: one that outlived its target keeps the times of an earlier sync.
///
/// A sibling a build holds is skipped, since cargo writes artifacts before their fingerprints.
/// A target is only ever reflinked: a full copy per tree would fill the disk, and a filesystem
/// that cannot share blocks gets no seed at all. Sources are small and may live on another
/// filesystem than targets, so they fall back to a plain copy.
const SEED: &str = r#"ranked=$(for used in $(ls -t "$SCRATCH/target/{repo}/.dibs-used" "$SCRATCH/target/{repo}"-local-*/.dibs-used 2>/dev/null); do
    src=${used%/.dibs-used}
    n=0
    if [ -s "${DIBS_PKGS:-/nonexistent}" ] && [ -f "$src/.dibs-packages" ]; then
        n=$(LC_ALL=C comm -12 "$DIBS_PKGS" "$src/.dibs-packages" | wc -l)
    fi
    echo "$n $src"
done | sort -s -k1,1nr)
while read -r n src; do
    [ -n "$src" ] || continue
    fds=
    held=0
    for lock in $(find "$src" -maxdepth 3 -name .cargo-lock 2>/dev/null); do
        exec {fd}<"$lock"
        fds="$fds $fd"
        flock -n -s "$fd" || { held=1; break; }
    done
    if [ "$held" = 0 ] && cp -a --reflink=always "$src" "$TARGET.seed.$$" 2>/dev/null && mv -T "$TARGET.seed.$$" "$TARGET" 2>/dev/null; then
        echo "DIBS-SEED ${src##*/}"
        printf '%s\n' "$WT" > "$TARGET/.dibs-tree"
        [ -s "${DIBS_PKGS:-/nonexistent}" ] && echo "DIBS-SEED-SHARED $n $(wc -l < "$DIBS_PKGS")"
        case "${src##*/}" in
            {repo}-local-*)
                sources=$SCRATCH/ws/{repo}/local-${src##*/{repo}-local-}
                # cubecl keeps autotune winners and throughput numbers in the tree's target/environment,
                # and a new tree must start without its sibling's.
                if [ -d "$sources" ] && cp -a --reflink=auto "$sources" "$WT.seed.$$" 2>/dev/null &&
                    rm -rf "$WT.seed.$$/target/environment" && mv -T "$WT.seed.$$" "$WT" 2>/dev/null; then
                    echo "DIBS-SEED-SOURCES"
                fi
                rm -rf "$WT.seed.$$" ;;
        esac
    fi
    for fd in $fds; do exec {fd}<&-; done
    rm -rf "$TARGET.seed.$$"
    [ -d "$TARGET" ] && break
done <<< "$ranked"
"#;

/// The tree's package list waits beside its target directory until a build step succeeds, so a
/// target is never credited with a lockfile whose build failed or never ran. Named per prepare:
/// two prepares of one target must not merge each other's lockfile. One never merged, because its
/// build failed, is gone after a day.
const STAGE: &str = r#"if [ -s "${DIBS_PKGS:-/nonexistent}" ]; then
    mv "$DIBS_PKGS" "$TARGET/.dibs-packages.pending.$DIBS_PKGS_TOKEN"
fi
find "$TARGET" -maxdepth 1 -name '.dibs-packages.pending.*' -mmin +1440 -delete 2>/dev/null || true
"#;

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
        if words.next().as_deref().map(|p| p.rsplit('/').next() == Some("cargo")) != Some(true) {
            continue;
        }
        let mut toolchain = String::new();
        let mut sub = words.next().unwrap_or_default();
        if let Some(t) = sub.strip_prefix('+') {
            toolchain = t.to_string();
            sub = words.next().unwrap_or_default();
        }
        if !matches!(sub.as_str(), "build" | "b" | "test" | "t" | "bench" | "run" | "r" | "nextest") {
            continue;
        }
        let words: Vec<String> = words.collect();
        let mut profile = if sub == "bench" { "release".to_string() } else { "dev".to_string() };
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
                    } else if let Some(v) = w.strip_prefix("--features=").or_else(|| w.strip_prefix("-F")) {
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
        return Some(format!("{toolchain}|{profile}|{target}|{}|{}|{rustflags}", flags.join(" "), features.join(",")));
    }
    None
}

/// Written ahead of a setup script, as short sorted hashes the seed and the record compare. A git
/// package gets a line of its own, since a revision is what most often differs and costs most.
/// Registry packages share 64 lines, one per bucket of their contents, because the whole script
/// travels in one command-line argument and a line per package does not fit in it.
pub fn packages_script(lock: &str, signature: &str, token: &str) -> String {
    use sha2::{Digest, Sha256};
    let short = |text: &str| format!("{:.12}", hex(&Sha256::digest(format!("{signature}\n{text}").as_bytes())));
    let mut lines = std::collections::BTreeSet::new();
    let mut buckets: Vec<Vec<String>> = vec![Vec::new(); 64];
    let mut take = |name: Option<String>, version: Option<String>, source: Option<String>| {
        let (Some(n), Some(v), Some(src)) = (name, version, source) else { return };
        let key = format!("{n} {v} {src}");
        if src.starts_with("git+") {
            lines.insert(short(&key));
        } else {
            let i = usize::from_str_radix(&short(&key)[..2], 16).unwrap_or(0) % 64;
            buckets[i].push(key);
        }
    };
    let (mut name, mut version, mut source) = (None, None, None);
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            take(name.take(), version.take(), source.take());
        } else if let Some(v) = line.strip_prefix("name = ") {
            name = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("version = ") {
            version = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("source = ") {
            source = Some(v.trim_matches('"').to_string());
        }
    }
    take(name, version, source);
    for (i, mut keys) in buckets.into_iter().enumerate() {
        if !keys.is_empty() {
            keys.sort();
            lines.insert(short(&format!("bucket {i}\n{}", keys.join("\n"))));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut s = "DIBS_PKGS_TOKEN=".to_string() + token + "\nDIBS_PKGS=${DIBS_SCRATCH:-$HOME/.cache/dibs}/.packages.$DIBS_PKGS_TOKEN\ntrap 'rm -f \"$DIBS_PKGS\"' EXIT\nmkdir -p \"${DIBS_PKGS%/*}\"\ncat > \"$DIBS_PKGS\" <<'DIBS_PACKAGES'\n";
    for l in lines {
        s.push_str(&l);
        s.push('\n');
    }
    s.push_str("DIBS_PACKAGES\n");
    s
}

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

{stage}{gc}git -C "$SRC" worktree prune

echo "DIBS-WT $WT"
echo "DIBS-TARGET $TARGET"
echo "DIBS-REV {repo} $SHORT"
"#,
        gc = GC,
        stage = STAGE
    );
    s
}

/// What follows a setup inside the same job.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Then {
    /// A recipe step, run in the worktree with the target exported.
    Step,
    /// rsync's far side, which owns stdout, into the worktree's name under its parent: the
    /// transfer names `local-<key>/`, so a tree that was never prepared cannot turn `--delete`
    /// on whatever directory the job happened to start in.
    Transfer,
}

/// A setup run at the head of the job that needs it, so a recipe pays for one round trip and
/// one place in the queue instead of two. Its report is what `parse` reads and ends with
/// `DIBS-READY`, or `DIBS-HELD` when a git dependency must be sent before anything can build.
/// `title` leads as a comment, since the first line is what `--status` and `--log` show of a job.
pub fn ahead(setup: &str, then: Then, title: &str) -> String {
    let fd = match then {
        Then::Step => 1,
        Then::Transfer => 2,
    };
    let title = title.replace(['\n', '\r'], " ");
    let mut s = format!(
        r#"# {title}
__dibs_report=$(mktemp "${{TMPDIR:-/tmp}}/dibs-prepare.XXXXXX") || exit 70
(
{setup}
) > "$__dibs_report"
__dibs_rc=$?
cat "$__dibs_report" >&{fd}
__dibs_wt=$(sed -n 's/^DIBS-WT //p' "$__dibs_report" | tail -n 1)
__dibs_target=$(sed -n 's/^DIBS-TARGET //p' "$__dibs_report" | tail -n 1)
__dibs_missing=$(grep -c '^DIBS-GITMISSING ' "$__dibs_report")
rm -f "$__dibs_report"
[ "$__dibs_rc" = 0 ] || exit "$__dibs_rc"
case $__dibs_wt in
    "${{DIBS_SCRATCH:-$HOME/.cache/dibs}}"/ws/?*) ;;
    *) echo "dibs: the setup reported no worktree under scratch, so nothing ran" >&2; exit 3 ;;
esac
"#
    );
    match then {
        Then::Step => s.push_str(
            "[ \"$__dibs_missing\" = 0 ] || { echo DIBS-HELD; echo 'dibs: a git dependency has to be sent first; the step runs once it is' >&2; exit 3; }\necho DIBS-READY\ncd -- \"$__dibs_wt\" || exit 3\nexport CARGO_TARGET_DIR=\"$__dibs_target\"\n",
        ),
        Then::Transfer => s.push_str("echo DIBS-READY >&2\ncd -- \"${__dibs_wt%/*}\" || exit 3\n"),
    }
    s
}

/// How a local tree is sent. `--checksum` without `--times` is what the seed relies on: a file
/// whose bytes match is left alone with the time it was copied with, and any other is rewritten
/// and takes the current time.
/// The marker is excluded so `--delete` leaves it, or collection could never date the tree.
pub const SYNC_ARGS: &[&str] = &["-rlpgo", "--checksum", "--no-times", "--delete", "--exclude=.git", "--exclude=/.dibs-used", "--filter=:- .gitignore"];

pub struct Prepared {
    pub worktree: String,
    pub target: String,
    pub revisions: Vec<(String, String)>,
    /// The sibling target directory this one was copied from, when it was.
    pub seeded: Option<String>,
    /// How many of this tree's packages that sibling had built, out of how many.
    pub seed_shared: Option<(usize, usize)>,
    /// Whether the sibling's sources came too, so unchanged files keep the times they were built at.
    pub seeded_sources: bool,
}

/// Reads the markers back out. Anything else the setup printed is left alone, so a fetch that
/// says something useful still reaches the caller.
pub fn parse(out: &str) -> Result<Prepared, String> {
    let mut worktree = None;
    let mut target = None;
    let mut revisions = Vec::new();
    let mut seeded = None;
    let mut seed_shared = None;
    let mut seeded_sources = false;
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("DIBS-WT ") {
            worktree = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("DIBS-TARGET ") {
            target = Some(v.trim().to_string());
        } else if line.trim() == "DIBS-SEED-SOURCES" {
            seeded_sources = true;
        } else if let Some(v) = line.strip_prefix("DIBS-SEED-SHARED ") {
            let mut it = v.split_whitespace().map(|x| x.parse::<usize>());
            if let (Some(Ok(a)), Some(Ok(b))) = (it.next(), it.next()) {
                seed_shared = Some((a, b));
            }
        } else if let Some(v) = line.strip_prefix("DIBS-SEED ") {
            seeded = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("DIBS-REV ") {
            let mut it = v.split_whitespace();
            if let (Some(r), Some(sha)) = (it.next(), it.next()) {
                revisions.push((r.to_string(), sha.to_string()));
            }
        }
    }
    match (worktree, target) {
        (Some(worktree), Some(target)) => Ok(Prepared { worktree, target, revisions, seeded, seed_shared, seeded_sources }),
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

    // Every local tree lost its marker to the sync's --delete, so none could ever be collected.
    #[test]
    fn a_tree_with_no_marker_is_dated_rather_than_kept_forever() {
        let (home, _, _) = sandbox("ws-unmarked");
        let lost = home.join("scratch/ws/other/local-unmarked");
        std::fs::create_dir_all(&lost).unwrap();
        targets(&home, "5", &setup_script("demo", "local-only"));
        assert!(lost.join(".dibs-used").exists());
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
{stage}{gc}echo "DIBS-WT $WT"
echo "DIBS-TARGET $TARGET"
echo "DIBS-REV {repo} local:{content}"
"#,
        gc = GC,
        stage = STAGE,
        seed = SEED.replace("{repo}", repo)
    );
    s
}

#[cfg(test)]
mod local_tests {
    use super::*;
    use std::process::Command;

    fn run_ahead(scratch: &std::path::Path, setup: &str, then: Then, after: &str) -> (i32, String) {
        std::fs::create_dir_all(scratch).unwrap();
        let out = Command::new("bash")
            .arg("-c")
            .arg(ahead(setup, then, "a step") + after)
            .current_dir(scratch)
            .env("DIBS_SCRATCH", scratch)
            .env("TMPDIR", scratch)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        (out.status.code().unwrap_or(-1), text)
    }

    #[test]
    fn a_step_after_its_setup_runs_in_the_prepared_tree_with_its_target() {
        let scratch = tmp("ahead-step");
        let (code, out) = run_ahead(&scratch, &setup_local_script("demo", "k1", "c"), Then::Step, "echo \"at $PWD with $CARGO_TARGET_DIR\"\n");
        assert_eq!(code, 0, "{out}");
        let wt = scratch.join("ws/demo/local-k1");
        let parsed = parse(&out).unwrap();
        assert_eq!(parsed.worktree, wt.display().to_string());
        let ready = out.find("DIBS-READY").expect("the report ends before the step");
        let step = out.find("at ").unwrap();
        assert!(ready < step);
        assert!(out.contains(&format!("at {} with {}", wt.display(), scratch.join("target/demo-local-k1").display())), "{out}");
        assert!(std::fs::read_dir(&scratch).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with("dibs-prepare")), "the report file is removed");
    }

    #[test]
    fn nothing_runs_after_a_setup_that_failed_or_named_no_tree() {
        let scratch = tmp("ahead-fail");
        for (setup, want) in [("exit 5", 5), ("echo DIBS-WT /somewhere/else", 3), ("true", 3)] {
            for then in [Then::Step, Then::Transfer] {
                let (code, out) = run_ahead(&scratch, setup, then, "echo RAN\n");
                assert_eq!(code, want, "{setup} {then:?}: {out}");
                assert!(!out.contains("RAN") && !out.contains("DIBS-READY"), "{setup} {then:?}: {out}");
            }
        }
    }

    #[test]
    fn a_transfer_starts_beside_the_tree_it_names() {
        let scratch = tmp("ahead-transfer");
        let (code, out) = run_ahead(&scratch, &setup_local_script("demo", "k2", "c"), Then::Transfer, "echo \"at $PWD\" >&2\n");
        assert_eq!(code, 0, "{out}");
        assert!(out.contains(&format!("at {}", scratch.join("ws/demo").display())), "{out}");
    }

    #[test]
    fn a_step_waits_while_a_git_dependency_is_missing() {
        let scratch = tmp("ahead-held");
        let setup = setup_local_script("demo", "k3", "c") + "echo 'DIBS-GITMISSING widget-0123456789abcdef deadbeef'\n";
        let (code, out) = run_ahead(&scratch, &setup, Then::Step, "echo RAN\n");
        assert_eq!(code, 3, "{out}");
        assert!(out.contains("DIBS-HELD") && !out.contains("DIBS-READY") && !out.contains("RAN"), "{out}");
    }

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
        prepare_local_with(scratch, key, hold, cp, "", "t0")
    }

    fn prepare_local_with(scratch: &std::path::Path, key: &str, hold: Option<&str>, cp: &str, lock: &str, token: &str) -> String {
        let script = packages_script(lock, SIG, token) + &setup_local_script("demo", key, "c");
        let bin = scratch.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let real = String::from_utf8(Command::new("bash").args(["-c", "type -P cp"]).output().unwrap().stdout).unwrap();
        let shim = match cp {
            "reflinks" => format!("#!/bin/bash\nargs=()\nshared=0\nfor a; do case \"$a\" in --reflink=always|--reflink=auto) shared=1 ;; *) args+=(\"$a\") ;; esac; done\n[ $shared = 1 ] || exit 1\nexec {} \"${{args[@]}}\"\n", real.trim()),
            // Targets on a filesystem with reflinks, sources on one without, as on a machine
            // whose target directories alone were moved.
            "targets only" => format!("#!/bin/bash\nargs=()\nmode=\nfor a; do case \"$a\" in --reflink=*) mode=$a ;; *) args+=(\"$a\") ;; esac; done\ncase \"$mode:${{args[-1]}}\" in --reflink=auto:*/ws/*|--reflink=always:*/target/*) exec {} \"${{args[@]}}\" ;; esac\nexit 1\n", real.trim()),
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn sibling(scratch: &std::path::Path) -> std::path::PathBuf {
        let t = scratch.join("target/demo-local-old");
        std::fs::create_dir_all(t.join("debug/deps")).unwrap();
        std::fs::write(t.join("debug/deps/libdep.rlib"), "artifact\n").unwrap();
        std::fs::write(t.join("debug/.cargo-lock"), "").unwrap();
        std::fs::write(t.join(".dibs-used"), "").unwrap();
        t
    }

    const SIG: &str = "|release||--no-default-features|cpu,fusion|";

    const LOCK_A: &str = "[[package]]\nname = \"cubecl\"\nversion = \"0.11.0\"\nsource = \"git+https://github.com/tracel-ai/cubecl?rev=aaa#aaa\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    fn packages_of(lock: &str) -> String {
        packages_script(lock, SIG, "t0").lines().filter(|l| l.len() == 12).map(|l| format!("{l}\n")).collect()
    }

    #[test]
    fn a_lockfile_becomes_sorted_hashes_and_a_revision_is_a_different_package() {
        let a = packages_of(LOCK_A);
        assert_eq!(a.lines().count(), 2, "one git package, one registry bucket, no workspace member");
        let mut sorted: Vec<&str> = a.lines().collect();
        sorted.sort();
        assert_eq!(sorted, a.lines().collect::<Vec<_>>());
        let b = packages_of(&LOCK_A.replace("rev=aaa#aaa", "rev=bbb#bbb"));
        assert_eq!(a.lines().filter(|l| b.contains(*l)).count(), 1);
        let bumped = packages_of(&LOCK_A.replace("version = \"1.0.0\"", "version = \"1.0.1\""));
        assert_eq!(a.lines().filter(|l| bumped.contains(*l)).count(), 1);
        assert_eq!(packages_script("", SIG, "t0"), "");
        let debug = packages_script(LOCK_A, "|dev||||", "t0");
        assert_eq!(a.lines().filter(|l| debug.contains(*l)).count(), 0, "another profile shares nothing");
    }

    fn age(path: &std::path::Path) -> u64 {
        std::fs::metadata(path).unwrap().modified().unwrap().elapsed().map(|d| d.as_secs()).unwrap_or(0)
    }

    /// A local sibling tree whose two source files were last written long ago.
    fn sibling_sources(scratch: &std::path::Path) -> std::path::PathBuf {
        let ws = scratch.join("ws/demo/local-old");
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/same.rs"), "fn same() {}\n").unwrap();
        std::fs::write(ws.join("src/edited.rs"), "fn before() {}\n").unwrap();
        Command::new("touch").args(["-d", "400 days ago"]).arg(ws.join("src/same.rs")).arg(ws.join("src/edited.rs")).status().unwrap();
        ws
    }

    // The whole point, and the rsync behaviour it rests on: after the sync, a file this tree did
    // not change is still dated before the copied artifacts, and a changed one is dated after.
    #[test]
    fn after_the_sync_only_changed_files_are_newer_than_the_copied_build() {
        let scratch = tmp("seed-sources");
        sibling(&scratch);
        sibling_sources(&scratch);
        let out = prepare_local(&scratch, "new", None, "reflinks");
        let p = parse(&out).unwrap();
        assert!(p.seeded_sources, "{out}");
        let wt = std::path::PathBuf::from(&p.worktree);
        let mine = scratch.join("mine");
        std::fs::create_dir_all(mine.join("src")).unwrap();
        std::fs::write(mine.join("src/same.rs"), "fn same() {}\n").unwrap();
        std::fs::write(mine.join("src/edited.rs"), "fn after() {}\n").unwrap();
        let st = Command::new("rsync").args(SYNC_ARGS).arg(format!("{}/", mine.display())).arg(format!("{}/", wt.display())).status().unwrap();
        assert!(st.success());
        assert!(age(&wt.join("src/same.rs")) > 86400 * 300, "an unchanged file keeps the sibling's time");
        assert!(age(&wt.join("src/edited.rs")) < 3600, "a changed file is rewritten and dated now");
        assert_eq!(std::fs::read_to_string(wt.join("src/edited.rs")).unwrap(), "fn after() {}\n");
        assert!(wt.join(".dibs-used").exists(), "the sync must not delete the collection marker");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn sources_are_copied_even_where_only_targets_can_be_reflinked() {
        let scratch = tmp("seed-sources-plain");
        sibling(&scratch);
        sibling_sources(&scratch);
        let p = parse(&prepare_local(&scratch, "new", None, "targets only")).unwrap();
        assert_eq!(p.seeded.as_deref(), Some("demo-local-old"));
        assert!(p.seeded_sources);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // A fetched ref's shared target has no single tree its artifacts were built from.
    #[test]
    fn a_target_from_a_fetched_ref_brings_no_sources() {
        let scratch = tmp("seed-fetched");
        let shared = scratch.join("target/demo");
        std::fs::create_dir_all(shared.join("debug")).unwrap();
        std::fs::write(shared.join(".dibs-used"), "").unwrap();
        std::fs::create_dir_all(scratch.join("ws/demo/abc123")).unwrap();
        let p = parse(&prepare_local(&scratch, "new", None, "reflinks")).unwrap();
        assert_eq!(p.seeded.as_deref(), Some("demo"));
        assert!(!p.seeded_sources);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // Otherwise the sibling's claim comes with the copy and the first build dates every source,
    // rebuilding the crates the seed was there to keep.
    #[test]
    fn a_seeded_target_is_claimed_for_the_tree_it_was_seeded_for() {
        let scratch = tmp("seed-claim");
        std::fs::write(sibling(&scratch).join(".dibs-tree"), "/the/sibling\n").unwrap();
        sibling_sources(&scratch);
        let p = parse(&prepare_local(&scratch, "new", None, "reflinks")).unwrap();
        assert_eq!(std::fs::read_to_string(std::path::Path::new(&p.target).join(".dibs-tree")).unwrap().trim(), p.worktree);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_seeded_tree_does_not_start_with_its_sibling_s_autotune_store() {
        let scratch = tmp("seed-environment");
        sibling(&scratch);
        let store = sibling_sources(&scratch).join("target/environment/default");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("autotune"), "winners\n").unwrap();
        let p = parse(&prepare_local(&scratch, "new", None, "reflinks")).unwrap();
        assert!(p.seeded_sources);
        let wt = std::path::Path::new(&p.worktree);
        assert!(wt.join("src/same.rs").exists() && !wt.join("target/environment").exists());
        assert!(store.join("autotune").exists(), "the sibling keeps its own");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A tree whose one source was written long ago, and the target it builds into.
    fn tree_and_target(name: &str, claimed_by: Option<&str>) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let scratch = tmp(name);
        let (wt, target) = (scratch.join("ws/demo/abc"), scratch.join("target/demo"));
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(wt.join("src/lib.rs"), "fn f() {}\n").unwrap();
        Command::new("touch").args(["-d", "400 days ago"]).arg(wt.join("src/lib.rs")).status().unwrap();
        if let Some(c) = claimed_by {
            let c = if c == "this" { wt.canonicalize().unwrap().display().to_string() } else { c.to_string() };
            std::fs::write(target.join(".dibs-tree"), c + "\n").unwrap();
        }
        (scratch, wt, target)
    }

    fn in_tree(wt: &std::path::Path, target: &std::path::Path, script: &str) -> (i32, String) {
        let out = Command::new("bash").arg("-c").arg(script).current_dir(wt).env("CARGO_TARGET_DIR", target).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
    }

    #[test]
    fn a_build_after_another_tree_s_dates_this_tree_s_sources_after_it() {
        let (scratch, wt, target) = tree_and_target("claim-other", Some("/another/tree"));
        let (code, out) = in_tree(&wt, &target, &claiming("echo BUILT"));
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("BUILT") && out.contains("did not make the last build"), "{out}");
        assert!(age(&wt.join("src/lib.rs")) < 3600);
        let claimed = std::fs::read_to_string(target.join(".dibs-tree")).unwrap();
        assert_eq!(claimed.trim(), wt.canonicalize().unwrap().display().to_string());
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
            assert_eq!(out.lines().any(|l| l == "DIBS-REFUSED"), want == 78, "{claimed_by:?}: {out}");
            let _ = std::fs::remove_dir_all(&scratch);
        }
    }

    #[test]
    fn the_sibling_that_built_the_most_of_this_lockfile_wins_over_a_newer_one() {
        let scratch = tmp("seed-match");
        let old = sibling(&scratch);
        std::fs::write(old.join(".dibs-packages"), packages_of(LOCK_A)).unwrap();
        let newer = scratch.join("target/demo-local-newer");
        std::fs::create_dir_all(newer.join("debug")).unwrap();
        std::fs::write(newer.join(".dibs-packages"), packages_of(&LOCK_A.replace("rev=aaa#aaa", "rev=bbb#bbb"))).unwrap();
        Command::new("touch").arg("-d").arg("400 days ago").arg(old.join(".dibs-used")).status().unwrap();
        std::fs::write(newer.join(".dibs-used"), "").unwrap();
        let out = prepare_local_with(&scratch, "new", None, "reflinks", LOCK_A, "t1");
        let p = parse(&out).unwrap();
        assert_eq!(p.seeded.as_deref(), Some("demo-local-old"), "{out}");
        assert_eq!(p.seed_shared, Some((2, 2)));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn among_equal_matches_the_newest_sibling_wins() {
        let scratch = tmp("seed-tie");
        let old = sibling(&scratch);
        Command::new("touch").arg("-d").arg("400 days ago").arg(old.join(".dibs-used")).status().unwrap();
        // Named to sort after the older sibling, so only recency can put it first.
        let newer = scratch.join("target/demo-local-zz-newer");
        std::fs::create_dir_all(newer.join("debug")).unwrap();
        std::fs::write(newer.join(".dibs-used"), "").unwrap();
        let out = prepare_local_with(&scratch, "new", None, "reflinks", LOCK_A, "t1");
        assert_eq!(parse(&out).unwrap().seeded.as_deref(), Some("demo-local-zz-newer"), "{out}");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Runs a step through `recording`, without the `cd` and export dibs puts around it.
    fn step(scratch: &std::path::Path, key: &str, token: &str, run: &str) {
        let target = scratch.join(format!("target/demo-local-{key}"));
        let script = format!("export CARGO_TARGET_DIR={}\n{}", target.display(), recording(run, token));
        Command::new("bash").args(["-c", &script]).status().unwrap();
    }

    fn record(scratch: &std::path::Path, key: &str) -> Option<String> {
        std::fs::read_to_string(scratch.join(format!("target/demo-local-{key}/.dibs-packages"))).ok()
    }

    #[test]
    fn a_target_is_credited_with_a_lockfile_only_once_a_build_succeeds() {
        let scratch = tmp("record-success");
        prepare_local_with(&scratch, "k", None, "no reflinks", LOCK_A, "t1");
        assert_eq!(record(&scratch, "k"), None, "a prepare alone records nothing");
        step(&scratch, "k", "t1", "false");
        assert_eq!(record(&scratch, "k"), None, "a failed build records nothing");
        step(&scratch, "k", "t1", "true; exit 0");
        assert_eq!(record(&scratch, "k").unwrap().lines().count(), 2, "a command that exits itself still records");
        assert!(std::fs::read_dir(&scratch).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".packages.")));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_wrapped_step_keeps_the_commands_exit_status() {
        let scratch = tmp("record-exit");
        std::fs::create_dir_all(scratch.join("target/demo-local-k")).unwrap();
        let script = format!("export CARGO_TARGET_DIR={}\n{}", scratch.join("target/demo-local-k").display(), recording("exit 7", "t1"));
        assert_eq!(Command::new("bash").args(["-c", &script]).status().unwrap().code(), Some(7));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // Two agents prepare one target and the first one's build finishes after the second prepare.
    #[test]
    fn a_build_merges_only_what_its_own_prepare_staged() {
        let scratch = tmp("record-own");
        prepare_local_with(&scratch, "k", None, "no reflinks", LOCK_A, "t1");
        prepare_local_with(&scratch, "k", None, "no reflinks", &LOCK_A.replace("rev=aaa#aaa", "rev=bbb#bbb"), "t2");
        step(&scratch, "k", "t1", "true");
        assert_eq!(record(&scratch, "k").unwrap(), packages_of(LOCK_A));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_setup_that_fails_leaves_no_package_list_behind() {
        let scratch = tmp("record-leak");
        std::fs::create_dir_all(&scratch).unwrap();
        let script = packages_script(LOCK_A, SIG, "t1") + "exit 3\n";
        let out = Command::new("bash").args(["-c", &script]).env("DIBS_SCRATCH", &scratch).status().unwrap();
        assert_eq!(out.code(), Some(3));
        assert!(std::fs::read_dir(&scratch).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".packages.")));
        let _ = std::fs::remove_dir_all(&scratch);
    }

    // The union, not the last lockfile: artifacts from an earlier revision are still there.
    #[test]
    fn a_target_remembers_every_lockfile_built_into_it() {
        let scratch = tmp("record");
        prepare_local_with(&scratch, "k", None, "no reflinks", LOCK_A, "t1");
        step(&scratch, "k", "t1", "true");
        prepare_local_with(&scratch, "k", None, "no reflinks", &LOCK_A.replace("rev=aaa#aaa", "rev=bbb#bbb"), "t2");
        step(&scratch, "k", "t2", "true");
        assert_eq!(record(&scratch, "k").unwrap().lines().count(), 3);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_signature_is_what_a_build_depends_on_besides_the_lockfile() {
        let a = build_signature("start=$(date +%s); cargo build --release -p app --no-default-features --features cpu,fusion; rc=$?").unwrap();
        let b = build_signature("cargo build -p app --features fusion,cpu --release --no-default-features").unwrap();
        assert_eq!(a, b, "flag order and feature order do not matter");
        assert_eq!(a, "|release||--no-default-features|cpu,fusion|");
        assert_ne!(a, build_signature("cargo build -p app --no-default-features --features cpu,fusion").unwrap());
        assert_eq!(build_signature("cargo bench --no-run").unwrap(), "|release||||");
        assert_eq!(build_signature("cargo +nightly bench").unwrap(), "nightly|release||||");
        assert_eq!(build_signature("cargo test --profile=ci -Fa --target x86_64-unknown-linux-gnu").unwrap(), "|ci|x86_64-unknown-linux-gnu||a|");
        assert_eq!(build_signature("RUSTFLAGS=-Ctarget-cpu=native ~/.cargo/bin/cargo build").unwrap(), "|dev||||-Ctarget-cpu=native");
        assert_eq!(build_signature("cargo build && ./target/debug/app --features x --release").unwrap(), "|dev||||", "flags of a later command are not cargo's");
        assert_eq!(build_signature("cargo run --release -- --features x").unwrap(), "|release||||", "arguments after -- go to the program");
    }

    #[test]
    fn commands_that_leave_no_usable_artifacts_record_nothing() {
        for run in ["cargo fmt --check", "cargo clippy --release", "cargo check", "cargo --version", "./target/release/app bench"] {
            assert_eq!(build_signature(run), None, "{run}");
        }
    }

    #[test]
    fn a_new_tree_starts_from_its_repos_latest_target() {
        let scratch = tmp("seed");
        sibling(&scratch);
        let out = prepare_local(&scratch, "new", None, "reflinks");
        assert_eq!(parse(&out).unwrap().seeded.as_deref(), Some("demo-local-old"), "{out}");
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
