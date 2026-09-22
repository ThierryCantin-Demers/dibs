# A holder file outlives its process only if that process was killed outright, since the
# lock itself is the open descriptor. Trust the pid, not the file.
# A job of a cancelled batch that is holding the lock loses what runs under it and then exits
# 76 itself, so the lock is released the ordinary way. One still queued is killed outright,
# since its flock would otherwise return and the command would run unlocked. The mark refuses
# the batch's later steps here for a day, which is what stops a driver on another computer.
kill_batch_here() {   # id anyone
    local id=$1 anyone=$2 f pid mode start label agent who stopped=0 sig victim
    local -a jobs=()
    for f in "$DIR"/batch.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r bid _ < "$f"
        [ "$bid" = "$id" ] && jobs+=("${f##*.}")
    done
    for pid in "${jobs[@]}"; do
        f=$DIR/holder.$pid; [ -e "$f" ] || f=$DIR/waiting.$pid
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode _ start label agent who _ < "$f"
        if [ -z "$anyone" ] && { case "$who" in shell-*) true ;; *) [ -n "$who" ] && [ -n "$AGENT_ID" ] && [ "$who" != "$AGENT_ID" ] ;; esac; }; then
            echo "dibs: batch $id is running $label for $agent, not for you. If it should stop:" >&2
            echo "  dibs --kill $id --anyone" >&2
            exit 2
        fi
    done
    : > "$DIR/cancelled.$id"
    [ "$MODE" = kill-force ] && sig=KILL || sig=TERM
    for pid in "${jobs[@]}"; do
        if [ -e "$DIR/holder.$pid" ]; then
            for victim in $(tree_below "$pid" | tac); do kill -"$sig" "$victim" 2>/dev/null; done
        elif [ -e "$DIR/waiting.$pid" ]; then
            for victim in $(printf '%s\n' "$pid" $(tree_below "$pid") | tac); do kill -"$sig" "$victim" 2>/dev/null; done
        else
            continue
        fi
        stopped=$((stopped+1))
    done
    CMD_ONE="cancelled batch $id: $stopped job(s) stopped"
    LABEL=$id
    log_event cancelled
    echo "Cancelled batch $id on $(hostname -s): stopped $stopped job(s), and its later steps are refused here."
    exit 0
}

# A pid names one process only while it has the same start time. pid_max comes round in days on a
# busy machine, so a record left behind by a job that was killed outright, before its own cleanup
# could run, would otherwise name whoever holds that pid by then.
started_at() {   # pid; when it started, in seconds since the epoch
    local rest
    rest=$(sed 's/.*) //' "/proc/$1/stat" 2>/dev/null) || return 1
    [ -n "$rest" ] || return 1
    [ -n "${BTIME:-}" ] || BTIME=$(awk '/^btime/{print $2}' /proc/stat 2>/dev/null)
    set -- $rest
    printf '%s' $(( BTIME + ${20} / CLK ))
}

# A record is written after the process it names started, so one whose process is younger than the
# record itself is the remains of a job that has ended. Two seconds of slack for the rounding.
still_the_same() {   # pid record-file
    local began wrote
    began=$(started_at "$1") || return 1
    wrote=$(stat -c %Y "$2" 2>/dev/null) || return 0
    [ "$began" -le $(( wrote + 2 )) ]
}

prune() {
    local f pid
    find "$DIR" -maxdepth 1 -name 'cancelled.*' -mmin +1440 -delete 2>/dev/null
    for f in "$DIR"/port.*; do
        [ -e "$f" ] || continue
        pid=$(cat "$f" 2>/dev/null)
        { [ -n "$pid" ] && [ -d "/proc/$pid" ]; } || rm -f "$f"
    done
    for f in "$DIR"/holder.* "$DIR"/waiting.*; do
        [ -e "$f" ] || continue
        pid=${f##*.}
        still_the_same "$pid" "$f" || rm -f "$f"
    done
    for f in "$DIR"/cpu.* "$DIR"/batch.* "$DIR"/hold.* "$DIR"/with.*; do
        [ -e "$f" ] || continue
        pid=${f##*.}
        # /proc rather than kill -0: signalling another user's process fails with EPERM,
        # which would prune a live holder's record on a machine with more than one account.
        [ -d "/proc/$pid" ] || rm -f "$f"
    done
}

# A cumulative count can only answer whether a job has ever done anything, which catches one
# that wedged before it started and nothing else: the sweep that runs for an hour and then
# hangs has plenty of CPU behind it forever. Each look leaves behind what the tree had burned,
# so the next one can tell whether it has moved, and --watch leaves one every few seconds.
# Two observations are needed before there is anything to say, and it says nothing until then.
idle_check() {   # pid start; sets IDLE_FOR, IDLE_KIND and CPU_RATE
    local f=$DIR/cpu.$1 prev worked pts line=
    IDLE_FOR= IDLE_KIND= CPU_RATE=-1
    # 2>/dev/null ahead of the redirect, or the shell reports the first look at a holder,
    # when there is no sample yet, as an error.
    read -r line 2>/dev/null < "$f"
    read -r prev worked pts <<< "$line"
    case "${prev:-x}" in (*[!0-9]*) prev= ;; esac
    case "${worked:-x}" in (-) ;; (*[!0-9]*) worked= ;; esac
    case "${pts:-x}" in (*[!0-9]*) pts= ;; esac
    # Cores' worth, in hundredths, across the gap since the last look. The total on its own
    # says nothing about how hard a job is working: an hour of core-time means one thing over
    # ten minutes and quite another over ten hours.
    if [ -n "$prev" ] && [ -n "$pts" ] && [ "$NOW" -gt "$pts" ]; then
        CPU_RATE=$(( (CPU_TICKS - prev) * 100 / (CLK * (NOW - pts)) ))
    fi
    if [ "$CPU_TICKS" -eq 0 ]; then
        worked=-
    elif [ -z "$prev" ] || [ "$CPU_TICKS" -gt "$prev" ] || [ "$worked" = - ] || [ -z "$worked" ]; then
        worked=$NOW
    fi
    # Not rewritten within the same second, so two looks in quick succession still leave a
    # baseline far enough back to measure a rate against.
    [ "$NOW" -gt "${pts:-0}" ] && printf '%s %s %s\n' "$CPU_TICKS" "$worked" "$NOW" 2>/dev/null > "$f"
    if [ "$worked" = - ]; then
        IDLE_FOR=$(( NOW - $2 )); IDLE_KIND=never
    elif [ -n "$prev" ] && [ "$CPU_TICKS" -le "$prev" ]; then
        IDLE_FOR=$(( NOW - worked )); IDLE_KIND=stalled
    fi
    # A job still writing is a job still working, whatever its process tree says: a compiler
    # wrapper with a daemon, such as sccache, compiles outside the tree entirely, and reporting
    # that as stalled would have this tell you to kill a healthy build.
    if [ -n "$IDLE_FOR" ] && [ -n "${OUTPUT_FILE:-}" ] && [ -f "$OUTPUT_FILE" ]; then
        local wrote
        wrote=$(stat -c %Y "$OUTPUT_FILE" 2>/dev/null) || wrote=
        # Its own window, not the CPU one: they answer different questions, and sharing a
        # knob would mean tightening one silently disables the other.
        if [ -n "$wrote" ] && [ "$(( NOW - wrote ))" -lt "${DIBS_WROTE_WITHIN:-120}" ]; then
            IDLE_FOR= IDLE_KIND=
        fi
    fi
}

# Arrival order. It is what the queue looks like, not a promise about wake order. Insertion
# sort, since the queue is a handful of entries and sort(1) is a fork.
QF=()
queue_sorted() {
    local f j start rest
    local -a qs=()
    QF=()
    for f in "$DIR"/waiting.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r _ _ start rest < "$f"
        j=${#qs[@]}
        while [ "$j" -gt 0 ] && [ "${qs[j-1]}" -gt "$start" ]; do
            qs[j]=${qs[j-1]}; QF[j]=${QF[j-1]}; j=$((j-1))
        done
        qs[j]=$start; QF[j]=$f
    done
}

# Openers are not holders. A lock descriptor is inherited, and this can run inside a client
# that holds one, so the whole invocation asking the question shows up as openers: the client,
# the script it ships, and the children of the pipeline that calls fuser. Which of them holds
# the lock is in the descriptor itself, since the kernel reports a file's locks under fdinfo:
# /proc/locks cannot answer it, naming the flock helper that took the lock and then exited,
# never the process whose descriptor keeps it. A process group is a second guard, because an
# orphan is by definition the remains of a session that has gone. Beyond that, a queued client
# has a record, and a pid that has already exited holds nothing, which is the rule prune
# follows for the same reason.
tree_below() {   # pid; its descendants, parents before children, the pid itself excluded
    ps -eo pid=,ppid= | awk -v root="$1" '
        {child[NR]=$1; parent[NR]=$2}
        END {
            want[root]=1
            do {
                added=0
                for (i=1; i<=NR; i++)
                    if (want[parent[i]] && !want[child[i]]) {
                        want[child[i]]=1; printf "%s ", child[i]; added=1
                    }
            } while (added)
        }'
}

# The lock is released the moment a job exits, and a grandchild still running then runs unlocked
# beside the next measurement, so everything below goes first, then what was named: TERM, and
# KILL ten seconds later for whatever ignores it.
reap() {   # pids
    local round below p sig alive st
    for round in $(seq 50); do
        below=$(for p in "$@"; do tree_below "$p"; done)
        [ -n "$below" ] || break
        [ "$round" -gt 40 ] && sig=KILL || sig=TERM
        for p in $(printf '%s\n' $below | tac); do kill -"$sig" "$p" 2>/dev/null; done
        sleep 0.25
    done
    kill -TERM "$@" 2>/dev/null
    for round in $(seq 41); do
        # A zombie has ended, and outside its parent kill -0 would still find it.
        alive=$(for p in "$@"; do
                    st=$(sed 's/.*) //; s/ .*//' "/proc/$p/stat" 2>/dev/null)
                    [ -n "$st" ] && [ "$st" != Z ] && echo "$p"
                done)
        [ -n "$alive" ] || return 0
        [ "$round" = 41 ] && kill -KILL $alive 2>/dev/null
        sleep 0.25
    done
}

