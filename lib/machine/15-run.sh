# 8>&- 9>&- so the workload does not inherit the lock descriptors. A child that outlives
# its parent would otherwise keep the lock held with no holder record to show for it, and
# every later caller would queue behind something --status swears is not there.
# A transport job's stdin is rsync's protocol stream and has to reach it. Backgrounding is
# what makes that awkward: a background job in a non-interactive shell has its stdin
# redirected from /dev/null, so it is handed the channel on the descriptor the watch would
# otherwise have used.
# Every job owns a directory under scratch holding what ran and everything it printed. The
# output used to go down the ssh channel and nowhere else, so the moment a caller cut it
# with tail it was gone; now the whole of it is on the machine, readable with --out after
# the job has finished, and the caller is told where. Both streams land in one file, in
# order, the way a build log is read. Old directories go with the worktrees' own age limit.
JOBDIR=$SCRATCH/jobs/$JOB
JOBLOG=""
TEE=""
SINK=""
if [ "$MODE" != rsh ] && mkdir -p "$JOBDIR" 2>/dev/null; then
    printf '%s\n' "$CMD" > "$JOBDIR/cmd"
    export DIBS_JOB="$JOB"
    JOBLOG=$JOBDIR/log
    if [ "$STREAM" = 1 ]; then
        # Without the lock descriptors, or a tee outliving a killed job would hold the lock
        # with no holder record to show for it.
        mkfifo "$JOBDIR/pipe" 2>/dev/null && { tee "$JOBLOG" < "$JOBDIR/pipe" 8>&- 9>&- 5<&- & TEE=$!; } || JOBLOG=""
    else
        SINK=$JOBLOG
    fi
    find "$SCRATCH/jobs" -mindepth 1 -maxdepth 1 -mtime +"${DIBS_KEEP_DAYS:-14}" -exec rm -rf {} + 2>/dev/null 8>&- 9>&- 5<&- &
fi
RUN=$CMD
[ "$HOLD" = 1 ] && RUN="read -r st < $(printf %q "$DIR/hold.$$") && exit \"\$st\""
WITH_FAIL=""
if [ "${#PORT_NAME[@]}" -gt 0 ] && ! ports_take; then
    WITH_FAIL="no free port in ${DIBS_PORTS:-20500-20999} on $(hostname -s)"
    echo "dibs: $WITH_FAIL, so the command did not run." >&2
fi
[ -z "$WITH_FAIL" ] && [ "${#WITH_NAME[@]}" -gt 0 ] && { with_start; with_ready || with_stop; }
if [ -n "$WITH_FAIL" ]; then
    STATUS=77
    [ -n "$JOBLOG" ] && : >> "$JOBLOG"
    # A tee waiting for the job's output never gets a writer otherwise.
    [ -n "$TEE" ] && : > "$JOBDIR/pipe"
else
    if [ "$MODE" = rsh ]; then
        timeout --signal=TERM --kill-after=30 "$MAXHOLD" bash -c "$CMD" 8>&- 9>&- 0<&5 5<&- &
    elif [ -n "$JOBLOG" ] && [ "$MAXHOLD" -gt 0 ]; then
        timeout --signal=TERM --kill-after=30 "$MAXHOLD" bash -c "$RUN" 8>&- 9>&- 5<&- < /dev/null > "${SINK:-$JOBDIR/pipe}" 2>&1 &
    elif [ -n "$JOBLOG" ]; then
        bash -c "$RUN" 8>&- 9>&- 5<&- < /dev/null > "${SINK:-$JOBDIR/pipe}" 2>&1 &
    elif [ "$MAXHOLD" -gt 0 ]; then
        timeout --signal=TERM --kill-after=30 "$MAXHOLD" bash -c "$RUN" 8>&- 9>&- 5<&- < /dev/null &
    else
        bash -c "$RUN" 8>&- 9>&- 5<&- < /dev/null &
    fi
    WORK=$!
    echo "$WORK" >> "$WORKFILE"
    if [ "$HOLD" = 1 ]; then
        # The caller's command needs the ports too, and only this side knows what they are.
        ports=""
        for i in "${!PORT_NAME[@]}"; do ports="$ports ${PORT_NAME[$i]}=${PORT_NUM[$i]}"; done
        echo "DIBS-HOLDING$ports"
    fi
    if [ "$WITH_UP" = 1 ]; then
        wait -n -p ENDED "$WORK" "${WITH_PID[@]}"
        STATUS=$?
        if [ "$ENDED" != "$WORK" ]; then
            for i in "${!WITH_PID[@]}"; do
                [ "${WITH_PID[$i]}" = "$ENDED" ] &&
                    with_failed "$i" "exited $STATUS while the command ran" "the command was stopped"
            done
            reap "$WORK"
            wait "$WORK"
            STATUS=77
        fi
        with_stop
    else
        wait "$WORK"
        STATUS=$?
    fi
fi
[ -n "$BATCH_TAG" ] && [ -e "$DIR/cancelled.${BATCH_TAG%% *}" ] && STATUS=76
if [ -n "$TEE" ]; then
    wait "$TEE" 2>/dev/null
    rm -f "$JOBDIR/pipe"
fi
# It holds the channel open, and while it does the caller's ssh cannot return.
[ -n "$WATCHDOG" ] && kill "$WATCHDOG" 2>/dev/null
rm -f "$WORKFILE"
exec 5<&-
RUNTIME=$(( $(date +%s) - ACQUIRED ))
log_event finished "$WAITED" "$RUNTIME" "$STATUS"
LOGGED_END=1
