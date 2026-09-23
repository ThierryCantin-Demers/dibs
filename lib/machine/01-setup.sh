set -uo pipefail
# The call arrives as assignments ahead of this script, one per variable it reads; set -u makes
# a missing one an error here rather than an empty value further down.
: "$MODE" "$LABEL" "$WAIT" "$MAXHOLD" "$VERBOSE" "$JSON" "$CMD" "$NO_WATCH" "$TTY" "$HOLD" \
  "$DEV_PCI" "$DEV_RT" "$DEV_NAME" "$DEV_CHIP" "$DEV_TWINS" "$STREAM" "$MAXFROM" "$FINGERPRINT"
AGENT=$(printf %s "$AGENT" | tr '\n\t' '  ' | cut -c1-48)
AGENT_ID=$(printf %s "$AGENT_ID" | tr '\n\t' '  ' | cut -c1-48)
BATCH_TAG=$(printf '%s\n' "$BATCH" | head -1 | tr '\t' ' ')
case "$LEASE" in ''|*[!0-9]*) LEASE=0 ;; esac
case "$READY_WITHIN" in ''|*[!0-9]*) READY_WITHIN=300 ;; esac
PORT_NUM=()
WITH_PID=(); WITH_LOG=(); WITH_END=(); WITH_BAD=(); WITH_UP=0
[ -n "$AGENT" ] || AGENT=?
# The caller cannot clean this up: its command line is read by fish, which has no $?.
# Unlinking a running script is safe; bash keeps the open inode.
trap 'rm -f "$0"' EXIT

# Where every user on this machine meets. A lock under /run/user or /tmp is keyed by uid, so
# two people each took their own, each was told the machine was idle, and both benchmarked at
# once: a wrong answer with nothing to notice it by. The shared directory has to be created
# once by root, which is why this prefers it and does not require it. Absent, the old per-user
# path is used unchanged, which is correct on a machine with one user and is what --check
# reports on a machine that may not stay that way.
#
# Still tmpfs, so it empties on reboot. That is not cosmetic: a record left in a persistent
# directory could name a pid that a later boot has reused, and prune, which asks /proc whether
# the pid is alive, would believe it.
# Overridable because /dev/shm is a Linux convention rather than a guarantee, and because a
# machine may want it elsewhere. It is a location, not a switch: pointing it at a directory
# that does not exist falls back exactly as if it were unset.
SHARED_DIR=${DIBS_SHARED_LOCK_DIR:-/dev/shm/dibs-lock}
if [ -n "${DIBS_LOCK_DIR:-}" ]; then
    DIR=$DIBS_LOCK_DIR; LOCK_SCOPE=explicit
elif [ -d "$SHARED_DIR" ] && [ -w "$SHARED_DIR" ]; then
    DIR=$SHARED_DIR; LOCK_SCOPE=shared
else
    DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/dibs-lock; LOCK_SCOPE=peruser
fi
mkdir -p "$DIR" 2>/dev/null || { DIR=/tmp/dibs-lock-$(id -u); LOCK_SCOPE=peruser; mkdir -p "$DIR"; }
# A lock nobody can write to is not a lock. mkdir -p succeeds on a directory that is already
# there whether or not it can be written, which is what a sandboxed shell sees: the real
# directory, read-only. Every write inside then fails, flock reports a bad file descriptor,
# the guards fall through one by one and the work runs unlocked beside whatever is being
# measured, reporting success. Falling back to another directory would be no better here,
# because the sessions that can write to this one would go on excluding each other and not us.
if ! : > "$DIR/.writable.$$" 2>/dev/null; then
    echo "dibs: $DIR cannot be written, so no lock can be taken. Nothing was run." >&2
    echo "  A sandboxed shell is the usual cause: it sees the lock directory and cannot" >&2
    echo "  write in it. Unlocked work beside a measurement is the one outcome this exists" >&2
    echo "  to prevent, and another directory would not exclude the sessions using this one." >&2
    exit 71
fi
rm -f "$DIR/.writable.$$" 2>/dev/null
# Records have to be removable by whoever prunes them, not only by whoever wrote them, or one
# user's dead job wedges the queue for everyone else.
[ "$LOCK_SCOPE" = shared ] && umask 002

# Timings outlive the machine, so they live off the tmpfs. Shared alongside the lock when there
# is a shared place to put them: an estimate built from everyone's runs of a label is a better
# estimate, and a log that only shows your own jobs cannot answer who is holding the machine.
SHARED_STATE=${DIBS_SHARED_STATE_DIR:-/var/lib/dibs}
if [ -n "${DIBS_HISTORY:-}" ] || [ -n "${DIBS_LOG:-}" ]; then
    HIST=${DIBS_HISTORY:-${XDG_STATE_HOME:-$HOME/.local/state}/dibs/history}
    LOG=${DIBS_LOG:-${XDG_STATE_HOME:-$HOME/.local/state}/dibs/log}
elif [ -d "$SHARED_STATE" ] && [ -w "$SHARED_STATE" ]; then
    HIST=$SHARED_STATE/history; LOG=$SHARED_STATE/log
else
    HIST=${XDG_STATE_HOME:-$HOME/.local/state}/dibs/history
    LOG=${XDG_STATE_HOME:-$HOME/.local/state}/dibs/log
fi
mkdir -p "$(dirname "$HIST")" "$(dirname "$LOG")" 2>/dev/null

# Every arrival and every outcome, so a job that was killed, wedged or orphaned still
# leaves a trace. The duration history next door only records what succeeded, which is
# precisely why the jobs worth investigating are the ones missing from it.
LOGGED_END=0
