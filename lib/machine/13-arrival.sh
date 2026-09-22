START=$(date +%s)
printf -v JOB '%(%Y%m%d%H%M%S)T-%s' "$START" "$$"
# One record is one line. A multi-line command would otherwise be counted once per line
# and write a file the size of the script it is running.
CMD_ONE=$(printf %s "$CMD" | tr '\n\t' '  ' | cut -c1-200)
[ "${#CMD}" -gt 200 ] && CMD_ONE="$CMD_ONE …"
[ "$HOLD" = 1 ] && CMD_ONE="held for a command run elsewhere: $CMD_ONE"
# Deleting gigabytes is as much IO as writing them, so a sweep queues behind a measurement
# rather than competing with one, and the log carries what it reclaimed like any other job.
if [ "$MODE" = gc ]; then
    read -r GC_DAYS GC_DRY <<< "$CMD"
    CMD=$(gc_script "${GC_DAYS:-default}" "${GC_DRY:-0}")
    CMD_ONE="dibs --gc"
    [ "${GC_DAYS:-default}" = default ] || CMD_ONE="$CMD_ONE --days $GC_DAYS"
    [ "${GC_DRY:-0}" = 0 ] || CMD_ONE="$CMD_ONE --dry-run"
fi
# The fifo is also how --status tells a hold, which waits on purpose, from a job that is idle.
[ "$HOLD" = 1 ] && ! mkfifo "$DIR/hold.$$" && { echo "dibs: could not make $DIR/hold.$$, so nothing is held." >&2; exit 71; }
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$MODE" "$$" "$START" "$LABEL" "$AGENT" "$AGENT_ID" "${DEV_NAME:--}" "$CMD_ONE" > "$DIR/waiting.$$"
[ -n "$BATCH" ] && printf '%s\n' "$BATCH" > "$DIR/batch.$$"
log_event arrived
# A cap nobody chose follows this job's own history, so work that always runs long is not killed
# at its mode's default, and the caller hears the cap now rather than as exit 124. Its own, down
# to the procedure where the recipe layer named one: a cap taken from a run of the same label that
# does a quarter of the work is how a legitimate run is killed at 124.
if [ "$MAXFROM" = default ] && [ "$HOLD" = 0 ] && [ "$MAXHOLD" -gt 0 ] && { [ "$MODE" = bench ] || [ "$MODE" = shared ]; } &&
    estimate "$MODE" "$LABEL" "$AGENT" "$FINGERPRINT" && [ "$EST_SCOPE" = this ] && [ "$EST_N" -ge 3 ] &&
    [ $(( EST_HI * 2 )) -gt "$MAXHOLD" ]; then
    dur_ CAP_WAS "$MAXHOLD"; dur_ CAP_P90 "$EST_HI"
    MAXHOLD=$(( EST_HI * 2 )); dur_ CAP_NOW "$MAXHOLD"
    echo "dibs: 90% of $EST_N runs of this took up to $CAP_P90, so it may hold the lock for $CAP_NOW rather than $CAP_WAS. --max sets it." >&2
fi
# An end line even when the job is torn down, so the log never just stops mid-story.
# Nothing can be written if it is SIGKILLed, which is itself worth knowing when reading it.
# Every exit, not just the happy one: while the watch lives it holds the channel open and
# the caller's ssh cannot return, so giving up on a busy lock would hang the caller.
trap 'rm -f "$DIR/waiting.$$" "$DIR/holder.$$" "$DIR/work.$$" "$DIR/cpu.$$" "$DIR/batch.$$" "$DIR/hold.$$" "$DIR/with.$$" "$0"
      [ "$WITH_UP" = 1 ] && kill -TERM "${WITH_PID[@]}" 2>/dev/null
      for p in ${PORT_NUM[*]:-}; do rm -f "$DIR/port.$p"; done
      [ -n "$WATCHDOG" ] && kill "$WATCHDOG" 2>/dev/null
      [ "$LOGGED_END" = 1 ] || log_event aborted' EXIT

# stdin is the ssh channel, and nothing else reads it, so its EOF is how this side learns
# the caller is gone. tailscaled's ssh server does not turn a closed channel into a hangup,
# so waiting for one is not an option.
#
# It starts before the queue, not after it: a caller can die while its job is waiting, and
# a job whose caller is gone should leave the queue rather than hold a place for twenty
# minutes and then die the instant it is let in.
#
# A background job in a non-interactive shell has its stdin redirected from /dev/null, so
# the watch has to be handed the channel on another descriptor or it reads EOF at once and
# kills the job it is supposed to be protecting.
exec 5<&0
WORKFILE=$DIR/work.$$
MAIN=$$
WATCHDOG=""
if [ "$NO_WATCH" != 1 ]; then
# A caller that is alive says something at least once a lease, and one that sleeps closes nothing,
# so silence counts as gone.
{ while :; do
      if [ "$LEASE" -gt 0 ]; then read -r -t "$LEASE" -u 5 line; else read -r -u 5 line; fi
      rc=$?
      [ "$rc" = 0 ] || break
      # A hold ends with its caller saying how the command went, which is not the caller going away.
      if [ "$HOLD" = 1 ] && [ "${line%% *}" = release ]; then
          st=${line#release }; case "$st" in ''|*[!0-9]*) st=1 ;; esac
          printf '%s\n' "$st" > "$DIR/hold.$MAIN"
          exit 0
      fi
  done 2>/dev/null
  work=""
  for p in $(cat "$WORKFILE" 2>/dev/null); do kill -0 "$p" 2>/dev/null && work="$work $p"; done
  if [ "$rc" -gt 128 ]; then CMD_ONE="caller silent for ${LEASE}s: $CMD_ONE"; else CMD_ONE="caller gone: $CMD_ONE"; fi
  log_event caller-gone
  if [ -n "$work" ]; then
      reap $work
  else
      kill -TERM "$MAIN" 2>/dev/null   # still queueing: stop waiting for a lock nobody wants
  fi
} &
WATCHDOG=$!
disown "$WATCHDOG" 2>/dev/null   # or bash announces "Terminated" when we tear it down
fi

