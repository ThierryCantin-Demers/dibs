# The one thing a caller gets that is the same shape every time: what ran, how long it
# waited and ran, how it ended and who ended it, and where the whole of its output is. A
# pipe on the caller's side cannot cut this off, since it is on stderr, and an exit code a
# filter replaced is still here.
if [ -n "$JOBLOG" ]; then
    by=command
    [ "$STATUS" -eq 124 ] && [ "$MAXHOLD" -gt 0 ] && by=dibs
    [ "$STATUS" -eq 77 ] && [ -n "$WITH_FAIL" ] && by=dibs
    [ "$STATUS" -eq 76 ] && [ -n "$BATCH_TAG" ] && [ -e "$DIR/cancelled.${BATCH_TAG%% *}" ] && by=dibs
    [ "$STATUS" -eq 78 ] && grep -qx DIBS-REFUSED "$JOBLOG" 2>/dev/null && by=dibs
    lines=$(wc -l < "$JOBLOG" 2>/dev/null || echo 0)
    # The digest: enough to see how it went, never so much that a caller has to cut it.
    if [ "$STREAM" != 1 ]; then
        head=${DIBS_DIGEST_HEAD:-20} tail_=${DIBS_DIGEST_TAIL:-20}
        if [ "$lines" -le $(( head + tail_ + 5 )) ]; then
            cat "$JOBLOG"
        else
            head -n "$head" "$JOBLOG"
            printf '\n... %s lines omitted. The whole log:  dibs --on %s --out %s  (%s)\n\n' \
                "$(( lines - head - tail_ ))" "$(hostname -s)" "$JOB" "$JOBLOG"
            tail -n "$tail_" "$JOBLOG"
        fi
    fi
    # What cargo compiled, when it ran, because "Finished" above a benchmark with nothing
    # compiled is the sentence that says the numbers are for the previous binary. Read from the
    # log rather than the command, since what runs cargo may be a runner the command starts.
    built=""
    if grep -q '^ *Finished .*target(s) in ' "$JOBLOG" 2>/dev/null; then
        n=$(grep -c '^ *Compiling ' "$JOBLOG" 2>/dev/null); n=${n:-0}
        [ "$n" -gt 0 ] && built="  built=$n" || built="  built=nothing"
    fi
    printf 'job %s  %s  %s  queued %ss  ran %ss  exit %s  by=%s%s\n' \
        "$JOB" "$MODE" "$LABEL" "$WAITED" "$RUNTIME" "$STATUS" "$by" "$built" >&2
    # A hold's command printed where it ran, so the log here has nothing in it.
    [ "$HOLD" = 1 ] || printf '  log %s:%s  (%s lines)  dibs --on %s --out %s\n' "$(hostname -s)" "$JOBLOG" "$lines" "$(hostname -s)" "$JOB" >&2
    for i in "${!PORT_NAME[@]}"; do
        printf '  port %s: %s on %s\n' "${PORT_NAME[$i]}" "${PORT_NUM[$i]:-none free}" "$(hostname -s)" >&2
    done
    for i in "${!WITH_NAME[@]}"; do
        printf '  with %s: %s  log %s:%s\n' "${WITH_NAME[$i]}" "${WITH_END[$i]:-not started}" "$(hostname -s)" "${WITH_LOG[$i]}" >&2
    done
    [ "$built" = "  built=nothing" ] &&
        echo "  built nothing: cargo compiled 0 crates, so a measurement after this measures the previous binary." >&2
    # The same failing command re-run unchanged fails the same way, and the log shows it done
    # several times over. Said once, in the trailer, when the previous attempt is minutes old.
    if [ "$STATUS" -ne 0 ] && [ "$HOLD" = 0 ]; then
        prev=""
        # Only jobs inside the window are read at all: two weeks of them is thousands of
        # directories, and comparing each one made every failure take seconds to report.
        window=${DIBS_REPEAT_WINDOW:-900}
        while read -r d; do
            [ "$d" != "$JOBDIR" ] && [ -f "$d/meta" ] && cmp -s "$d/cmd" "$JOBDIR/cmd" || continue
            age=$(( $(date +%s) - $(stat -c %Y "$d/meta" 2>/dev/null || echo 0) ))
            [ "$age" -le "$window" ] || continue
            e=$(awk -F'\t' '$1=="exit"{print $2}' "$d/meta")
            [ "$e" != 0 ] && prev="${d##*/} exit $e, $(dur "$age") ago"
        done < <(find "$SCRATCH/jobs" -mindepth 1 -maxdepth 1 -type d -mmin -$(( window / 60 + 1 )) 2>/dev/null | sort)
        [ -n "$prev" ] && echo "  this exact command already failed here: job $prev. Unchanged, it failed the same way." >&2
    fi
    printf 'mode\t%s\nlabel\t%s\nqueued\t%s\nran\t%s\nexit\t%s\nby\t%s\nagent\t%s\nlines\t%s\n' \
        "$MODE" "$LABEL" "$WAITED" "$RUNTIME" "$STATUS" "$by" "$AGENT" "$lines" > "$JOBDIR/meta"
fi
if [ "$(wc -l < "$LOG" 2>/dev/null || echo 0)" -gt 20000 ]; then
    tail -10000 "$LOG" > "$LOG.tmp" && mv "$LOG.tmp" "$LOG"
fi
# Say what to do about it, not only what happened. A cold build of a large workspace can pass
# half an hour on its own, and the answer is almost always to run the same thing again: the
# target directory survives, so a compile picks up from the crates that finished rather than
# starting over. Raising --max is for the job that genuinely needs longer in one go.
if [ "$STATUS" -eq 124 ]; then
    echo "dibs: stopped after holding the lock for ${MAXHOLD}s, which is --max for a $MODE job." >&2
    echo "  Nothing is wrong with it; it was simply told to hold no longer than that." >&2
    [ "$HOLD" = 1 ] || {
        echo "  Run it again; a compile picks up from the crates that already finished, since the" >&2
        echo "  build cache outlives the job. Anything else starts over." >&2
    }
    echo "  If it truly needs one long run, say so:  --max $(( MAXHOLD * 2 ))" >&2
fi

# What the next caller's estimate is built from, so only runs that did what they set out
# to do belong in it. A benchmark that failed in a second, or was killed for overrunning,
# is not a typical duration: recording it teaches every later caller the wrong number.
if [ "$STATUS" -eq 0 ]; then
    printf '%s\t%s\t%s\t%s\t%s\n' "$MODE" "$LABEL" "$(( $(date +%s) - ACQUIRED ))" "$AGENT" "$FINGERPRINT" >> "$HIST"
    if [ "$(wc -l < "$HIST")" -gt 1000 ]; then
        tail -500 "$HIST" > "$HIST.tmp" && mv "$HIST.tmp" "$HIST"
    fi
fi
exit "$STATUS"
