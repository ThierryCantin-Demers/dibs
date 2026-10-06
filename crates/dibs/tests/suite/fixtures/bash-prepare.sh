# A prepare as a client that has not updated sends it on switch day: master's setup_script for
# app@main, with no slot, nest or fresh paths, rendered whole. It runs under bash with HOME and
# DIBS_SCRATCH set.
set -eu
SCRATCH=${DIBS_SCRATCH:-${DIBS_SCRATCH:-$HOME/.cache/dibs}}
SRC=$HOME/prog/app
[ -d "$SRC/.git" ] || { echo "dibs: no clone at $SRC" >&2; exit 3; }

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
if FETCHERR=$(git -C "$SRC" fetch -q origin "+main:$MINE" 2>&1); then
    SHA=$(git -C "$SRC" rev-parse --verify -q "$MINE^{commit}" || true)
    git -C "$SRC" update-ref -d "$MINE" 2>/dev/null || true
else
    # A bare commit cannot be fetched by name from most servers, and a branch that exists only
    # on this machine cannot be fetched at all. Both resolve locally, by their own name, which
    # is not a shared slot and cannot be overwritten by anyone else.
    FETCHERR="$FETCHERR
$(git -C "$SRC" fetch -q --all 2>&1 || true)"
    SHA=$(git -C "$SRC" rev-parse --verify -q 'main^{commit}' || true)
fi
# A fetch that failed on credentials means this machine cannot see the remote at all, and the
# ref it was asked for is very likely fine. Reported as "no such ref" it reads as a mistake in
# the ref, and the way out of that reading is to carry the code over by hand, which is a whole
# afternoon of bundle or tarball for something @local already does.
[ -n "$SHA" ] || {
    echo "dibs: no such ref in app: main" >&2
    case "$FETCHERR" in
      *"could not read Username"*|*"Authentication failed"*|*"terminal prompts disabled"*|\
      *"Permission denied (publickey)"*|*"Repository not found"*)
        echo "  The fetch failed on credentials, so nothing here can see that remote: a private" >&2
        echo "  repo is the usual reason, and the ref itself is probably fine." >&2
        echo "  Send your working tree instead, which fetches nothing:  app@local" >&2 ;;
    esac
    exit 3; }
SHORT=$(printf %s "$SHA" | cut -c1-12)

WT=$SCRATCH/ws/app/$SHORT
mkdir -p "${WT%/*}"
# Two prepares of one commit both see no tree and both add it, and the loser dies on "already
# exists". Per repo rather than per commit, because the prune below touches every worktree.
exec 7>"$SCRATCH/ws/app/.prepare.lock"
flock 7
# An add that was stopped leaves the tree registered and locked, with no index and files missing.
if [ -e "$WT/.git" ] && ! (cd "$WT" && [ -f "$(git rev-parse --git-path index 2>/dev/null)" ]); then
    git -C "$SRC" worktree remove --force --force "$WT" 2>/dev/null || rm -rf "$WT"
    git -C "$SRC" worktree prune
fi
if [ ! -d "$WT/.git" ] && [ ! -f "$WT/.git" ]; then
    # Detached on purpose: a worktree that tracks a branch would move under a job that is
    # still measuring from it.
    git -C "$SRC" worktree add --detach -q "$WT" "$SHA" 2>/dev/null || {
        # A tree left behind by a crash is registered but absent; prune and retry once.
        git -C "$SRC" worktree prune
        git -C "$SRC" worktree add --detach -q "$WT" "$SHA"; }
fi
exec 7>&-
touch "$WT/.dibs-used"

# One cache per repo rather than per tree. Cargo fingerprints per crate, so switching commits
# reuses most of it, where a tree of its own would rebuild the world every commit. Concurrent
# builds serialise on cargo's own lock, which is correct.
TARGET=$SCRATCH/target/app
mkdir -p "$TARGET" "$SCRATCH/out"
: > "$TARGET/.dibs-used"

if [ -s "${DIBS_PKGS:-/nonexistent}" ]; then
    mv "$DIBS_PKGS" "$TARGET/.dibs-packages.pending.$DIBS_PKGS_TOKEN"
fi
find "$TARGET" -maxdepth 1 -name '.dibs-packages.pending.*' -mmin +1440 -delete 2>/dev/null || true
KEEP=${DIBS_KEEP_DAYS:-14}
for old in "$SCRATCH"/ws/*/*; do
    [ -d "$old" ] || continue
    [ "$old" = "$WT" ] && continue
    if [ ! -e "$old/.dibs-used" ]; then touch "$old/.dibs-used"; continue; fi
    [ -n "$(find "$old/.dibs-used" -maxdepth 0 -mtime +"$KEEP" 2>/dev/null)" ] || continue
    echo "DIBS-GC $old" >&2
    git -C "$old" worktree remove --force "$old" 2>/dev/null || rm -rf "$old" 2>/dev/null ||
        echo "dibs: could not remove all of $old; the next sweep tries again" >&2
done
TKEEP=${DIBS_TARGET_KEEP_DAYS:-5}
for old in "$SCRATCH/target"/*; do
    [ -d "$old" ] || continue
    [ "$old" = "$TARGET" ] && continue
    # A prepare's marker is empty and a sweep's is not, so a target holding only a sweep's marker
    # was being deleted when a sweep dated it, and is no cache waiting for its first build.
    if [ -s "$old/.dibs-used" ] && [ -z "$(find "$old" -mindepth 1 -maxdepth 1 ! -name .dibs-used -print -quit 2>/dev/null)" ]; then
        rm -f "$old/.dibs-used"; rmdir "$old" 2>/dev/null || true; continue
    fi
    if [ ! -e "$old/.dibs-used" ]; then echo swept > "$old/.dibs-used"; continue; fi
    [ -n "$(find "$old/.dibs-used" -maxdepth 0 -mtime +"$TKEEP" 2>/dev/null)" ] || continue
    busy=0
    for lock in $(find "$old" -maxdepth 3 -name .cargo-lock 2>/dev/null); do
        flock -n -x "$lock" true || { busy=1; break; }
    done
    [ "$busy" = 0 ] || continue
    echo "DIBS-GC $old ($(du -sh "$old" 2>/dev/null | cut -f1))" >&2
    # A sweep is someone else's housekeeping, and must never be why this tree did not arrive.
    rm -rf "$old" 2>/dev/null || echo "dibs: could not remove all of $old; the next sweep tries again" >&2
done
git -C "$SRC" worktree prune

echo "DIBS-WT $WT"
echo "DIBS-TARGET $TARGET"
echo "DIBS-REV app $SHORT"
