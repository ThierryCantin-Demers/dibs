show_json() {
    local f mode pid start label agent cmd elapsed sep="" st=idle jl ja jc jo i=0 free=0 eta_known=1 jleft
    local c csep sc
    prune
    batch_tail
    now
    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode _ _ _ _ _ < "$f"
        [ "$mode" = bench ] && st=bench || st=shared
        break
    done
    if [ "$st" = idle ]; then
        exec 7>"$DIR/rw"
        if flock -n -x 7 2>/dev/null; then
            flock -u 7
        else
            # A handover is not an orphan, and calling it one here took the machine out of
            # routing for as long as anyone kept asking.
            lock_unaccounted
            [ -n "$ORPH" ] && st=orphan || st=busy
        fi
        exec 7>&-
    fi
    # What a dispatcher needs to rank this machine. loadavg rather than the sum of what dibs
    # holds, because on a machine someone is working at most of the competing load was never
    # started through dibs and summing holders cannot see it. Scaled by 100, since this is
    # bash and the reader wants an integer.
    printf '{"t":%s,"state":"%s","cores":%s,"load":%s' \
        "$NOW" "$st" "$(nproc 2>/dev/null || echo 1)" \
        "$(awk '{printf "%d", $1 * 100}' /proc/loadavg 2>/dev/null || echo 0)"
    # Which repos this machine has a build cache for. A dispatcher that does not know this
    # picks by load on the first run of a repo and lands somewhere with nothing cached, which
    # is minutes of cold compile chosen over seconds of queueing.
    printf ',"caches":['
    # Resolved here, not passed in: this half arrives over ssh, which forwards no environment.
    csep="" sc=${DIBS_SCRATCH:-$HOME/.cache/dibs}
    for c in "$sc/target"/*; do
        # A marker cargo writes, not the directory itself: preparing a worktree creates the
        # target directory whether or not anything is ever built in it, so without this every
        # machine claims every repo it has ever fetched.
        [ -f "$c/.rustc_info.json" ] || [ -d "$c/release" ] || [ -d "$c/debug" ] || continue
        printf '%s"%s"' "$csep" "$(basename "$c")"
        csep=","
    done
    printf ']'
    # Which repos a worktree can be prepared from at all. Separate from the cache above: a
    # cache makes a machine faster for a repo, a clone is what makes it possible, and a
    # dispatcher that cannot tell them apart sends work to a machine that fails at prepare.
    printf ',"clones":['
    lsep=""
    for c in "$HOME/prog"/*; do
        [ -d "$c/.git" ] || [ -f "$c/.git" ] || continue
        printf '%s"%s"' "$lsep" "$(basename "$c")"
        lsep=","
    done
    printf ']'
    printf ',"holders":['
    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent who dev cmd < "$f"
        elapsed=$(( NOW - start ))
        cpu_used "$pid"
        holder_output "$pid"
        idle_check "$pid" "$start"; [ -p "$DIR/hold.$pid" ] && IDLE_FOR=
        jstr jl "$label"; jstr ja "$agent"; jstr jc "$cmd"
        jstr jd "$dev"
        printf '%s{"mode":"%s","pid":%s,"label":%s,"agent":%s,"device":%s,"cmd":%s,"started":%s,"elapsed":%s,"cpu":%s' \
            "$sep" "$mode" "$pid" "$jl" "$ja" "$jd" "$jc" "$start" "$elapsed" "$CPU_USED"
        jleft=-1
        if estimate "$mode" "$label" "$agent"; then
            remaining_of "$elapsed"
            [ "$REM" -gt "$free" ] && free=$REM
            jleft=$REM
            [ "$REM" -lt 0 ] && eta_known=0
            printf ',"est":%s,"est_lo":%s,"est_hi":%s,"est_n":%s,"est_scope":"%s"' \
                "$EST_V" "$EST_LO" "$EST_HI" "$EST_N" "$EST_SCOPE"
            est_wide && printf ',"est_wide":true'
            [ "$REM" -ge 0 ] && printf ',"remaining":%s,"remaining_kind":"%s"' "$REM" "$REM_KIND"
            overrunning "$elapsed" && printf ',"overrun":true'
        else
            eta_known=0
        fi
        holder_output "$pid"
        [ -n "$OUTPUT_FILE" ] && { jstr jo "$OUTPUT_FILE"; printf ',"output":%s' "$jo"; }
        [ "$CPU_RATE" -ge 0 ] && printf ',"cpu_rate":%s' "$CPU_RATE"
        [ -n "$IDLE_FOR" ] && printf ',"idle_for":%s,"idle_kind":"%s"' "$IDLE_FOR" "$IDLE_KIND"
        batch_json "$pid" "$jleft"
        printf '}'
        sep=","
    done
    printf '],"queue":['
    queue_sorted
    sep=""
    queue_eta_start "$free" "$eta_known"
    for f in "${QF[@]}"; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent who dev cmd < "$f"
        i=$((i+1))
        jstr jl "$label"; jstr ja "$agent"; jstr jc "$cmd"; jstr jd "$dev"
        printf '%s{"position":%s,"mode":"%s","pid":%s,"label":%s,"agent":%s,"device":%s,"cmd":%s,"arrived":%s,"waiting":%s' \
            "$sep" "$i" "$mode" "$pid" "$jl" "$ja" "$jd" "$jc" "$start" "$(( NOW - start ))"
        queue_eta "$mode" "$label" "$agent"
        [ "$QE_ETA" -ge 0 ] && printf ',"eta":%s' "$QE_ETA"
        jleft=-1
        [ "$QE_ETA" -ge 0 ] && estimate "$mode" "$label" "$agent" && [ "$EST_SCOPE" = this ] && jleft=$(( QE_ETA + EST_V ))
        batch_json "$pid" "$jleft"
        printf '}'
        sep=","
    done
    printf ']}\n'
}

show() {
    local f held=0 mode pid start label cmd elapsed rem free=0 total=0 eta_known=1 sig=
    local used el ue uh rr wa idl agent AC MC left usual depth=0 qnote= jleft
    if [ "$VERBOSE" = 1 ]; then
        echo "  [$DIR]"
        ls -la "$DIR" 2>&1 | sed 's/^/  /'
        echo "  [$HIST: $( [ -s "$HIST" ] && wc -l < "$HIST" || echo 0 ) runs recorded]"
    fi
    prune
    for f in "$DIR"/holder.* "$DIR"/waiting.*; do sig="$sig ${f##*/}"; done
    [ "$sig" = "$EST_SIG" ] || { EST=(); EST_SIG=$sig; }
    batch_tail
    for f in "$DIR"/waiting.*; do [ -e "$f" ] && depth=$((depth+1)); done
    [ "$depth" -gt 0 ] && qnote="$C_DIM ($depth queued)$C_OFF"

    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent who dev cmd < "$f"
        [ "$dev" = - ] && dev=
        if [ "$held" -eq 0 ]; then
            [ "$mode" = bench ] && echo "${C_BUSY}dibs: BUSY, benchmark in progress${C_OFF}$qnote" \
                                || echo "${C_WARN}dibs: in use, shared${C_OFF}$qnote"
        fi
        held=$((held+1))
        now; elapsed=$(( NOW - start ))
        cpu_used "$pid"; used=$CPU_USED
        dur_ el "$elapsed"
        holder_output "$pid"
        idle_check "$pid" "$start"; [ -p "$DIR/hold.$pid" ] && IDLE_FOR=
        agent_hue "$agent"; mode_hue "$mode"
        if [ -n "$IDLE_FOR" ] && [ "$IDLE_FOR" -gt "${DIBS_IDLE_AFTER:-60}" ]; then
            dur_ idl "$IDLE_FOR"
            if [ "$IDLE_KIND" = never ]; then
                echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_WARN}[IDLE: no CPU at all in $idl, it is waiting on something]$C_OFF"
            else
                echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_WARN}[IDLE: ${used}s of CPU, none of it in the last $idl, it is waiting on something]$C_OFF"
            fi
            echo "    ${C_WARN}stop it with: dibs --kill $pid$C_OFF"
            echo "    $C_DIM$cmd$C_OFF"
            with_lines "$pid"
            echo "    ${C_DIM}from$C_OFF $AC$agent$C_OFF${dev:+${C_DIM} on $C_OFF$dev}"
            batch_lines "$pid" -1
            held=$((held+1))
            continue
        fi
        jleft=-1
        if estimate "$mode" "$label" "$agent"; then
            remaining_of "$elapsed"
            [ "$REM" -gt "$free" ] && free=$REM
            jleft=$REM
            if [ "$REM" -lt 0 ]; then
                eta_known=0; left="longer than it has ever taken"
            elif [ "$REM_KIND" = bound ]; then
                dur_ rr "$REM"; left="under $rr left if it runs true to form"
            else
                dur_ rr "$REM"; left="~$rr left"
            fi
            if est_wide; then
                dur_ ue "$EST_LO"; dur_ uh "$EST_HI"
                [ "$EST_LO" -eq 0 ] && ue="under a second"
                usual="anywhere from $ue to $uh over $EST_N runs"
            elif [ "$EST_V" -eq 0 ]; then
                usual="under a second over $EST_N runs"; left=
            elif [ "$EST_N" = 1 ]; then
                # One sample is a fact about one run, not a habit, and saying "usually" of it
                # claims a regularity nothing has been observed to have.
                dur_ ue "$EST_V"; usual="ran once, in $ue"
            else
                dur_ ue "$EST_V"; usual="usually $ue over $EST_N runs"
            fi
            # A colon rather than a verb: the phrase after it has to read the same whether it
            # says "usually 5m00s" or "anywhere from 0s to 4m39s".
            [ "$EST_SCOPE" = agent ] && usual="nothing on this one; this agent's other $mode jobs: $usual"
            [ "$EST_SCOPE" = mode ] && usual="nothing on this one; every $mode job on the machine: $usual"
            if overrunning "$elapsed"; then
                dur_ uh "$EST_HI"
                echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_WARN}[STUCK? its slowest run was $uh, this is over twice that]$C_OFF"
                echo "    ${C_WARN}stop it with: dibs --kill $pid$C_OFF"
            else
                echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_DIM}[$usual${left:+, $left}]$C_OFF"
            fi
        else
            # Without the holder's typical duration there is no honest ETA for the queue.
            eta_known=0
            echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_DIM}[no history for this one yet]$C_OFF"
        fi
        echo "    $C_DIM$cmd$C_OFF"
        with_lines "$pid"
        echo "    ${C_DIM}from$C_OFF $AC$agent$C_OFF${dev:+${C_DIM} on $C_OFF$dev}"
        batch_lines "$pid" "$jleft"
        # Where its output is going, so --out is discovered by reading the status rather than
        # by already knowing the feature exists. Silent when a job redirected nowhere, because
        # there is then nothing to offer and a line saying so would be noise on every tick.
        holder_output "$pid"
        [ -n "$OUTPUT_FILE" ] && \
            echo "    ${C_DIM}writing $OUTPUT_FILE$C_OFF   ${C_DIM}(dibs --on $(hostname -s) --out $pid)$C_OFF"
    done
    if [ "$held" -eq 0 ]; then
        exec 7>"$DIR/rw"
        if flock -n -x 7 2>/dev/null; then
            flock -u 7
            echo "${C_FREE}dibs: idle${C_OFF}"
        else
            lock_unaccounted
            if [ -n "$ORPH" ]; then
                echo "${C_BUSY}dibs: LOCKED BY AN ORPHAN.${C_OFF} No holder record, but the lock is taken, so"
                echo "  something outlived its parent. Nothing can run until it goes."
                # Named here rather than left as an instruction. This already runs on the
                # machine, so telling someone to go and ask it themselves is asking them to do
                # the one thing this could have done for them, at the moment they are least
                # able to.
                echo "  holding it:"
                for p in $ORPH; do
                    ps -o pid=,etime=,user=,args= -p "$p" 2>/dev/null |
                        sed 's/^ */    /' | cut -c1-100
                done
                echo "  Stop it with: dibs --kill <pid> --anyone"
            elif [ "$OPENERS" = 1 ]; then
                echo "${C_BUSY}dibs: busy${C_OFF}, a client has just taken the lock and is recording it."
            else
                echo "${C_BUSY}dibs: the lock is taken and nothing here reports holding it.${C_OFF}"
                echo "  If fuser is missing, install psmisc; without it an orphan cannot be named."
            fi
        fi
        exec 7>&-
    fi

    queue_sorted
    total=${#QF[@]}
    [ "$total" -eq 0 ] && return 0

    local i=0
    queue_eta_start "$free" "$eta_known"
    for f in "${QF[@]}"; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent who dev cmd < "$f"
        [ "$dev" = - ] && dev=
        i=$((i+1))
        age_ wa "$start"
        agent_hue "$agent"; mode_hue "$mode"
        queue_eta "$mode" "$label" "$agent"
        jleft=-1
        [ "$QE_ETA" -ge 0 ] && estimate "$mode" "$label" "$agent" && [ "$EST_SCOPE" = this ] && jleft=$(( QE_ETA + EST_V ))
        if [ "$QE_ETA" -gt 0 ]; then
            dur_ rr "$QE_ETA"
            echo "  ${C_Q}queued $i of $total$C_OFF: $MC$mode$C_OFF  $AC$label$C_OFF  waiting $C_B$wa$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_DIM}[~$rr until it starts]$C_OFF"
        elif [ "$QE_ETA" = 0 ]; then
            echo "  ${C_Q}queued $i of $total$C_OFF: $MC$mode$C_OFF  $AC$label$C_OFF  waiting $C_B$wa$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_DIM}[starts as soon as the lock frees]$C_OFF"
        else
            echo "  ${C_Q}queued $i of $total$C_OFF: $MC$mode$C_OFF  $AC$label$C_OFF  waiting $C_B$wa$C_OFF  ${C_DIM}pid $pid$C_OFF"
        fi
        echo "    $C_DIM$cmd$C_OFF"
        echo "    ${C_DIM}from$C_OFF $AC$agent$C_OFF${dev:+${C_DIM} on $C_OFF$dev}"
        batch_lines "$pid" "$jleft"
    done
    [ "$total" -gt 1 ] && echo "  $C_DIM(queued in arrival order; the kernel picks the actual wake order)$C_OFF"
    return 0
}

