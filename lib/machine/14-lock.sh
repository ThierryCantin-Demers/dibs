# Three quarters of shared jobs here finish in under five seconds, and behind a queued
# benchmark every one of them was waiting up to twenty minutes. A quick one may go around,
# but only while the benchmark has been waiting less than DIBS_PATIENCE and only if its own
# history says it will be gone within DIBS_QUICK, so the most this can cost a benchmark is
# the two added together.
#
# Its own history, not its mode's: the median across all shared jobs is under a second, which
# would wave through the four minute ones as readily as the instant ones.
#
# None of this can spoil a measurement. It decides who waits, nothing else: once a benchmark
# holds the lock, flock refuses every shared caller whatever this returns.
may_bypass() {
    [ "$MODE" = shared ] && [ "${DIBS_BYPASS:-1}" = 1 ] || return 1
    local f m p st queued=0
    now
    for f in "$DIR"/waiting.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r m p st _ < "$f"
        [ "$m" = bench ] || continue
        [ -d "/proc/$p" ] || continue
        queued=1
        [ $(( NOW - st )) -lt "${DIBS_PATIENCE:-60}" ] || return 1
    done
    [ "$queued" = 1 ] || return 1
    estimate "$MODE" "$LABEL" "$AGENT" "$FINGERPRINT" || return 1
    [ "$EST_SCOPE" = this ] || return 1   # what its agent usually takes is not what it takes
    [ "$EST_V" -le "${DIBS_QUICK:-10}" ]
}

exec 9>"$DIR/gate"
exec 8>"$DIR/rw"

# Everyone passes through the gate, and an exclusive waiter keeps holding it while it waits
# for the real lock. Without that, a steady trickle of shared users starves benchmarks.
if may_bypass; then
    log_event bypassed
elif [ -n "$WAIT" ]; then
    flock -x -w "$WAIT" 9 || {
        echo "dibs: busy, a benchmark is queued ahead of you. Gave up after ${WAIT}s." >&2
        show >&2
        exit 75
    }
else
    flock -x 9
fi
if [ "$MODE" = bench ]; then FLAG=-x; else FLAG=-s; fi

# Say so the moment it is going to wait, rather than going quiet for twenty minutes. A
# caller that cannot afford the wait can act on this; one that goes quiet gets killed by
# its own timeout and leaves the work undone with nothing to show why.
if ! flock -n $FLAG 8 2>/dev/null; then
    queued_line >&2
    [ "$VERBOSE" = 1 ] && show >&2
else
    flock -u 8   # let the real acquisition below take it under the same rules
fi

if [ -n "$WAIT" ]; then
    flock $FLAG -w "$WAIT" 8 || {
        flock -u 9
        echo "dibs: still busy after ${WAIT}s, gave up." >&2
        show >&2
        exit 75
    }
else
    flock $FLAG 8
fi
flock -u 9

WAITED=$(( $(date +%s) - START ))
ACQUIRED=$(date +%s)
# Restamped, not just renamed: a holder's clock starts when it acquires. Carrying the
# arrival time over would have --status report a job as running for the time it spent
# queued, and every number drawn from that elapsed, the ETA, the stuck check and the idle
# check, would be measuring a stretch the job spent doing nothing because it was waiting.
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$MODE" "$$" "$ACQUIRED" "$LABEL" "$AGENT" "$AGENT_ID" "${DEV_NAME:--}" "$CMD_ONE" > "$DIR/waiting.$$"
mv "$DIR/waiting.$$" "$DIR/holder.$$"
[ "$WAITED" -ge 5 ] && echo "dibs: acquired the $MODE lock after $(dur "$WAITED")" >&2

