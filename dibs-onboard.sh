#!/usr/bin/env bash
# Set up dibs on a machine that has never used it, and prove it works.
#
#   DIBS_MACHINE=<machine> bash dibs-onboard.sh /path/to/dibs [/path/to/dibs-run]
#
# Both are sent to you by whoever owns the machine, and both stay on your side: nothing is
# installed on the far side. `dibs` is one bash file and takes the lock; `dibs-run` is the
# interface you actually use, and is optional only in the sense that the machine works
# without it.
set -uo pipefail

MACHINE=${DIBS_MACHINE:-}
ACCOUNT=${DIBS_ACCOUNT:-dibs}
HOST="$ACCOUNT@$MACHINE"
BIN="$HOME/.local/bin"

say()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
fail() { printf '\n\033[31m%s\033[0m\n' "$*" >&2; exit 1; }

# No default machine. Guessing one and reporting that it is unreachable is a worse first
# five minutes than being told what is missing.
[ -n "$MACHINE" ] || fail "Which machine? Its owner will have told you.
  DIBS_MACHINE=<machine> bash $0 /path/to/dibs [/path/to/dibs-run]
  DIBS_ACCOUNT is '$ACCOUNT' unless you were told otherwise."

# --- 1. the script itself -------------------------------------------------------------
SRC=${1:-}
if [ -z "$SRC" ]; then
    [ -f "$BIN/dibs" ] || fail "Give me the dibs script: bash $0 /path/to/dibs"
else
    [ -f "$SRC" ] || fail "No such file: $SRC"
    mkdir -p "$BIN"
    install -m 755 "$SRC" "$BIN/dibs"
    say "installed $BIN/dibs"
fi
case ":$PATH:" in
    *":$BIN:"*) ;;
    *) fail "$BIN is not on your PATH. Add it, then run this again." ;;
esac

# --- 1b. the interface on top of it ---------------------------------------------------
RUN=${2:-}
if [ -n "$RUN" ]; then
    [ -f "$RUN" ] || fail "No such file: $RUN"
    install -m 755 "$RUN" "$BIN/dibs-run"
    say "installed $BIN/dibs-run"
fi
HAVE_RUN=0
[ -x "$BIN/dibs-run" ] && HAVE_RUN=1

# The default in the script is the owner's own alias, which does not exist here.
export DIBS_HOST="$HOST"

# --- 2. what is this machine ----------------------------------------------------------
# No separate reachability probe first. dibs already diagnoses an unreachable machine better
# than a fixed list of guesses can: it reads `tailscale status` and separates logged out from
# stopped from peer absent from peer offline from an ssh problem. The one thing it cannot know
# is the case below, which is specific to a first contact.
say "dibs --check"
if ! dibs --check; then
    fail "Cannot use $HOST. Read what --check said above first.

  If it reached the machine, send the output to its owner.
  If it did not, and Tailscale looks healthy and the machine is listed, the tailnet
  policy may not permit you yet. That is the owner's to fix, not yours."
fi

# --- 3. a shared job ------------------------------------------------------------------
# Shared is the default: builds, tests, inspection. Several people at once.
say "a shared job"
dibs --label onboard-hello 'echo "ran as $(id -un) on $(hostname -s)"; nvidia-smi -L' \
    || fail "the shared job failed"

# --- 4. the lock, actually doing something --------------------------------------------
# A benchmark takes the machine exclusively. Real work rather than a sleep, because a sleep
# under the exclusive lock stalls everyone else for its whole duration.
say "taking the exclusive lock for 25 seconds, then queueing behind it"
# Bounded in wall time, not in iterations. Sized by a count it finishes in four seconds on a
# fast machine and the queued job below never actually queues, which demonstrates nothing.
# Real arithmetic rather than a sleep: a sleep under the exclusive lock stalls everyone on the
# machine for its whole duration, and this is the worst possible place to teach that.
dibs --bench --label onboard-bench 'timeout 25 python3 -c "
while True: pass" >/dev/null 2>&1; echo "benchmark done (25s of real work)"' &
BENCH=$!

# Give it a moment to acquire before looking, bounded rather than open-ended. Each --status
# is an ssh round trip, so the network paces this at roughly one try a second on its own; the
# bound is there so a machine that never takes the job cannot spin here forever.
for _ in $(seq 15); do
    dibs --status 2>/dev/null | grep -q onboard-bench && break
done

echo
echo "--- what the machine looks like while it is held ---"
dibs --status

echo
echo "--- this shared job has to wait for it ---"
time dibs --label onboard-queued 'echo "I waited for the benchmark to finish"'

wait $BENCH

# --- 5. what to do next ---------------------------------------------------------------
say "working."

if [ "$HAVE_RUN" = 0 ]; then
    printf '\033[33m%s\033[0m\n' \
        "No dibs-run here. Everything below that starts with it will not run yet;" \
        "ask the machine's owner for the binary and pass it as the second argument."
fi

cat <<EOF

Make it permanent by setting the host in your shell, or every call goes to the wrong place:

  bash/zsh   echo 'export DIBS_HOST=$HOST' >> ~/.bashrc
  fish       set -Ux DIBS_HOST $HOST

Then, the interface. Work goes through dibs-run, which picks the worktree, the
build cache and the label for you, and records what actually ran:

  dibs-run list <repo>              what recipes that repo has
  dibs-run build <repo>@<ref> <r>   shared
  dibs-run test  <repo>@<ref> <r>   shared
  dibs-run bench <repo>@<ref> <r>   builds shared, measures exclusive
  dibs-run runs                     what has been measured, and what is comparable

For something with no recipe, so it still gets recorded rather than vanishing:

  dibs-run shell <repo>@<ref> --reason "..." -- '<cmd>'    in a worktree
  dibs-run raw --reason "..." -- '<cmd>'                   no repo
  dibs-run gaps                     the reasons that keep coming back. Those are
                                    the ones that should become recipes: tell the
                                    machine's owner what you see there.

dibs itself is the layer underneath. You want it for looking, and for the rare
command that fits nothing above:

  dibs --status             who holds it, who is queued, and for how long
  dibs --peek <command>     look without taking the lock. Free things only:
                            ps, nvidia-smi, ls, tail. It runs beside a benchmark.
  dibs --out                what a running job is writing
  dibs --log                what has run recently
  dibs <command>            shared, by hand
  dibs --bench <command>    exclusive, by hand
  dibs --help               everything, including --sync and --kill

Two things that matter more than the rest:

  Use --bench for anything you are going to believe a number from. It excludes
  shared jobs too, because a compile running beside a benchmark spoils it as
  surely as a second benchmark would.

  Prefer dibs-run over dibs <command>. Not style: a bare command files its
  duration under a label you invented once, so nothing ever looks it up and the
  ETAs stay useless, and it leaves no record of what was built from what. Both
  are why the estimates on this machine were bad for months.

Your scratch space is \$DIBS_SCRATCH on the machine. Put build trees there, never
in /tmp: that is a small tmpfs shared by everyone, and filling it breaks every
command on the machine including the ones for finding out why.
EOF
