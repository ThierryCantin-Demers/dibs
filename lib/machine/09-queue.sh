# One document per line, so a reader can take it a line at a time. Built with printf and
# parameter expansion rather than a JSON tool, because this runs on every --watch tick and a
# fork per field would undo the whole point of the redraw being free.
jstr() {   # var value
    local v=${2-}
    v=${v//\\/\\\\}
    v=${v//\"/\\\"}
    printf -v "$1" '"%s"' "$v"
}
# Two renderers asking the same question in two places is exactly how they come to disagree:
# --json called a job overrunning while --status said nothing, for every label whose recorded
# runs were all under a second and whose median is therefore zero.
# Judged against the slowest this label has ever honestly been, not against its middle: a
# label whose runs range twenty seconds to twelve minutes has no business calling a two
# minute run stuck.
overrunning() {   # elapsed
    [ "$EST_SCOPE" = this ] && [ "$EST_HI" -gt 0 ] && [ "$1" -gt $(( EST_HI * 2 )) ]
}

# Past the median the job is in the tail, and the tail still has a shape. Surrendering there
# cost every waiter its ETA, because the labels that hold the machine longest are exactly the
# ones whose median is zero. Zero remains a wrong answer, reading as "any moment now".
remaining_of() {   # elapsed; sets REM, -1 when nothing can be said, and REM_KIND
    REM_KIND=typical
    if [ "$1" -lt "$EST_V" ]; then REM=$(( EST_V - $1 ))
    elif [ "$1" -lt "$EST_HI" ]; then REM=$(( EST_HI - $1 )); REM_KIND=bound
    else REM=-1; REM_KIND=
    fi
}

# Queued shared jobs are not standing in line behind one another. The shared lock admits all
# of them the moment it is free, so a run of them starts together and the queue only actually
# advances at a benchmark, which has to wait for everything already admitted to drain.
# Adding their durations up told the third shared job it was waiting out the first two.
queue_eta_start() {   # free known
    QE_AT=$1 QE_KNOWN=$2 QE_PENDING=0 QE_PENDING_KNOWN=1
}
queue_eta() {   # mode label agent; sets QE_ETA, -1 when it cannot be said
    local have=1
    estimate "$1" "$2" "$3" || have=0
    if [ "$1" != bench ]; then
        [ "$QE_KNOWN" = 1 ] && QE_ETA=$QE_AT || QE_ETA=-1
        if [ "$have" = 1 ]; then
            [ "$EST_V" -gt "$QE_PENDING" ] && QE_PENDING=$EST_V
        else
            QE_PENDING_KNOWN=0   # only matters to a benchmark queued behind it
        fi
        return
    fi
    if [ "$QE_KNOWN" = 1 ] && [ "$QE_PENDING_KNOWN" = 1 ]; then
        QE_ETA=$(( QE_AT + QE_PENDING ))
        [ "$have" = 1 ] && QE_AT=$(( QE_ETA + EST_V )) || QE_KNOWN=0
    else
        QE_ETA=-1 QE_KNOWN=0
    fi
    QE_PENDING=0 QE_PENDING_KNOWN=1
}

# What a caller that has just been queued is told, in one line: what holds the machine, how many
# wait ahead of it and when it should start. The whole picture is dibs status, or -v here.
queued_line() {
    local f mode pid start label agent first= n=0 ahead=0 free=0 known=1 eta=-1 line d
    prune
    now
    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent _ < "$f"
        n=$((n+1))
        [ -n "$first" ] || { [ "$mode" = bench ] && first="the benchmark $label" || first=$label; }
        if estimate "$mode" "$label" "$agent"; then
            remaining_of $(( NOW - start ))
            if [ "$REM" -lt 0 ]; then known=0; elif [ "$REM" -gt "$free" ]; then free=$REM; fi
        else
            known=0
        fi
    done
    queue_sorted
    queue_eta_start "$free" "$known"
    for f in "${QF[@]}"; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent _ < "$f"
        queue_eta "$mode" "$label" "$agent"
        [ "$pid" = "$$" ] && { eta=$QE_ETA; break; }
        ahead=$((ahead+1))
    done
    # Nothing holds it on the record and yet it is taken: the wait ahead is not a queue but a
    # wedge, and it lasts until the orphan goes. Said here because this is the line the caller
    # reads before settling in for twenty minutes.
    if [ "$n" = 0 ]; then
        lock_unaccounted
        [ -n "$ORPH" ] && {
            printf 'dibs: the lock is held by an orphan (pid%s), which left no record, so nothing here can start. Reclaim it with: dibs --release\n' "$ORPH"
            return 0; }
    fi
    line="dibs: queued and has not started, behind ${first:-a job starting up}"
    [ "$n" -gt 1 ] && line="$line and $((n-1)) more"
    [ "$ahead" -gt 0 ] && line="$line, with $ahead queued first"
    if [ "$eta" -gt 0 ]; then dur_ d "$eta"; line="$line, ~$d until it starts"; fi
    printf '%s. dibs status shows the queue.\n' "$line"
}

# Where the queue will stand once everything waiting now has started, as queue_eta leaves it. A
# batch's steps still to come arrive after all of it, so they are placed behind it.
queue_tail() {
    local f mode start label agent free=0 known=1
    QT_EXCL=0
    now
    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode _ start label agent _ < "$f"
        [ "$mode" = bench ] && QT_EXCL=1
        if estimate "$mode" "$label" "$agent"; then
            remaining_of $(( NOW - start ))
            [ "$REM" -lt 0 ] && known=0
            [ "$REM" -gt "$free" ] && free=$REM
        else
            known=0
        fi
    done
    queue_sorted
    queue_eta_start "$free" "$known"
    for f in "${QF[@]}"; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode _ _ label agent _ < "$f"
        [ "$mode" = bench ] && QT_EXCL=1
        queue_eta "$mode" "$label" "$agent"
    done
    QT_AT=$QE_AT QT_KNOWN=$QE_KNOWN QT_PENDING=$QE_PENDING QT_PKNOWN=$QE_PENDING_KNOWN
}

batch_tail() {
    local f
    QT_AT=0 QT_KNOWN=0 QT_PENDING=0 QT_PKNOWN=0 QT_EXCL=0
    for f in "$DIR"/batch.*; do
        [ -e "$f" ] && { queue_tail; return; }
    done
}

# A batch reaches the machine one step at a time, so without its plan a job looks like the last
# thing its agent asked for, and the time that agent has left reads as this job's alone. Only a
# label's own history counts toward it: what an agent or the machine usually takes says nothing
# about a step that has never run.
batch_read() {   # pid left-of-this-job; sets BT_*, 1 when the job is not part of a batch
    local f=$DIR/batch.$1 name mode label here d start
    local at=${QT_AT:-0} known=${QT_KNOWN:-0} pend=${QT_PENDING:-0} pknown=${QT_PKNOWN:-0} excl=${QT_EXCL:-0}
    BT_ID= BT_STEP= BT_K= BT_N= BT_HERE=0 BT_NEXT= BT_FARN=0 BT_FAR= BT_LEFT=$2 BT_PARTIAL=0
    [ -s "$f" ] || return 1
    {
        IFS=$'\t' read -r BT_ID BT_STEP
        IFS=$'\t' read -r BT_K BT_N
        while IFS=$'\t' read -r name mode label here; do
            [ -n "$name" ] || continue
            if [ "$here" != 1 ]; then
                BT_FARN=$((BT_FARN+1))
                [ "$BT_FARN" -le 6 ] && BT_FAR="$BT_FAR${BT_FAR:+, }$name"
                continue
            fi
            BT_HERE=$((BT_HERE+1))
            if [ "$mode" = peek ]; then
                [ "$BT_HERE" -le 6 ] && BT_NEXT="$BT_NEXT${BT_NEXT:+, }$name"
                continue
            fi
            # A shared step waits only while a benchmark holds or is queued; a benchmark waits
            # for everything admitted before it. Where the queue cannot be estimated the step
            # is placed as if it were empty, and the total becomes a floor.
            start=$BT_LEFT
            if [ "$BT_LEFT" -ge 0 ]; then
                if [ "$mode" = bench ]; then
                    if [ "$known" = 1 ] && [ "$pknown" = 1 ]; then
                        [ $(( at + pend )) -gt "$start" ] && start=$(( at + pend ))
                    else
                        BT_PARTIAL=1
                    fi
                elif [ "$excl" = 1 ]; then
                    if [ "$known" = 1 ]; then
                        [ "$at" -gt "$start" ] && start=$at
                    else
                        BT_PARTIAL=1
                    fi
                fi
            fi
            if estimate "$mode" "$label" "" && [ "$EST_SCOPE" = this ]; then
                dur_ d "$EST_V"; d=" ~$d"
                if [ "$BT_LEFT" -ge 0 ]; then
                    BT_LEFT=$(( start + EST_V ))
                    if [ "$mode" = bench ]; then
                        at=$BT_LEFT pend=0 known=1 pknown=1
                    elif [ $(( BT_LEFT - at )) -gt "$pend" ]; then
                        pend=$(( BT_LEFT - at ))
                    fi
                fi
            else
                d=" (no history)"; BT_PARTIAL=1
                [ "$BT_LEFT" -ge 0 ] && BT_LEFT=$start
            fi
            [ "$BT_HERE" -le 6 ] && BT_NEXT="$BT_NEXT${BT_NEXT:+, }$name$d"
        done
    } < "$f"
    [ "$BT_HERE" -gt 6 ] && BT_NEXT="$BT_NEXT and $(( BT_HERE - 6 )) more"
    [ "$BT_FARN" -gt 6 ] && BT_FAR="$BT_FAR and $(( BT_FARN - 6 )) more"
    BT_K=${BT_K//[^0-9]/} BT_N=${BT_N//[^0-9]/}
    [ -n "$BT_ID" ]
}

batch_lines() {   # pid left-of-this-job
    batch_read "$1" "$2" || return 0
    local l
    echo "    ${C_DIM}batch$C_OFF $BT_ID${C_DIM}, step ${BT_K:-?} of ${BT_N:-?}: $BT_STEP$C_OFF"
    [ "$BT_HERE" -gt 0 ] && echo "    ${C_DIM}then here:$C_OFF $BT_NEXT"
    [ "$BT_FARN" -gt 0 ] && echo "    ${C_DIM}then on other machines:$C_OFF $BT_FAR"
    if [ "$BT_LEFT" -lt 0 ]; then
        echo "    ${C_DIM}batch time left here: unknown, this step's own time cannot be estimated$C_OFF"
    else
        dur_ l "$BT_LEFT"
        if [ "$BT_PARTIAL" = 1 ]; then
            echo "    ${C_DIM}batch time left here: over $l, since some of what is ahead has no history$C_OFF"
        else
            echo "    ${C_DIM}batch time left here: ~$l$C_OFF"
        fi
    fi
}

batch_json() {   # pid left-of-this-job
    batch_read "$1" "$2" || return 0
    local ji js jn jf
    jstr ji "$BT_ID"; jstr js "$BT_STEP"; jstr jn "$BT_NEXT"; jstr jf "$BT_FAR"
    printf ',"batch":{"id":%s,"step":%s,"k":%s,"n":%s,"here":%s,"elsewhere":%s,"next":%s,"far":%s' \
        "$ji" "$js" "${BT_K:-0}" "${BT_N:-0}" "$BT_HERE" "$BT_FARN" "$jn" "$jf"
    [ "$BT_LEFT" -ge 0 ] && printf ',"left":%s,"left_partial":%s' "$BT_LEFT" \
        "$([ "$BT_PARTIAL" = 1 ] && echo true || echo false)"
    printf '}'
}

