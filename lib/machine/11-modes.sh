case "$MODE" in
    status)  [ "$JSON" = 1 ] && { show_json; exit 0; }
             show; exit 0 ;;
    watch)
        # No lock, like --status, and no work between ticks: the read is both the interval
        # and the liveness check, since EOF on the channel is how this side hears that the
        # terminal watching it is gone.
        exec 5<&0
        now; HEARD=$NOW
        while :; do
            if [ "$JSON" = 1 ]; then
                show_json
            else
            [ "$TTY" = 1 ] && printf '\033[H\033[2J\033[3J'
            printf '%s   every %ss, ctrl-c to stop\n' "$(date '+%H:%M:%S')" "$LABEL"
            show
            fi
            if [ "$NO_WATCH" = 1 ]; then
                sleep "$LABEL"
            else
                # The interval is the tick. A word from the caller only says it is still there,
                # and redrawing on one would cost the machine a render every time it spoke and
                # give a reader ticks that are not the interval it asked for.
                now; due=$(( NOW + LABEL ))
                while [ "$NOW" -lt "$due" ]; do
                    read -r -t "$(( due - NOW ))" -u 5 _
                    rc=$?
                    now
                    if [ "$rc" = 0 ]; then HEARD=$NOW
                    elif [ "$rc" -le 128 ]; then exit 0
                    elif [ "$LEASE" -gt 0 ] && [ $(( NOW - HEARD )) -gt "$LEASE" ]; then exit 0
                    fi
                done
            fi
        done ;;
    log)     n=$LABEL
             [ -s "$LOG" ] || { echo "Nothing logged yet ($LOG)."; exit 0; }
             printf '%-19s %-9s %-7s %-14s %6s %6s %5s  %-21s  %-22s %s\n' \
                 WHEN EVENT MODE LABEL QUEUED RAN EXIT JOB AGENT COMMAND
             # Lines written before events carried a job id name the process instead.
             tail -n "$n" "$LOG" | awk -F'\t' '{
                 printf "%-19s %-9s %-7s %-14s %6s %6s %5s  %-21s  %-22s %s%s\n",
                        substr($1,1,19), $2, $4, substr($5,1,14), $6, $7, $8,
                        (NF >= 12 && $12 != "-" ? $12 : "pid " $3),
                        substr(($10 == "" ? "?" : $10),1,22), substr($9,1,50),
                        (NF >= 11 && $11 != "-" ? "  [batch " $11 "]" : "")
             }'
             exit 0 ;;
    # What decides whether a binary built here can run somewhere else. Reported as facts
    # rather than as one hash: compatibility is directional, and a hash can only say that two
    # machines differ, never which of them can accept the other's work.
    abi)
        printf 'kernel %s\n' "$(uname -s)"
        printf 'arch %s\n' "$(uname -m)"
        abi_facts
        command -v rustc >/dev/null 2>&1 && printf 'rustc %s\n' "$(rustc -V 2>/dev/null | awk '{print $2}')"
        nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null |
            head -1 | awk 'NF { print "nvidia " $1 }'
        printf 'os %s\n' "$(os_name)"
        exit 0 ;;

    check)
        # Getting here at all has already proved the parts that cannot be tested directly: ssh
        # reached the machine, its login shell parsed the bootstrap line, and bash ran what
        # arrived. What follows is everything that would otherwise fail later and further away.
        FAIL=0 WARN=0
        ok()   { printf '  ok    %s\n' "$1"; }
        warn() { printf '  warn  %s\n' "$1"; WARN=$((WARN+1)); }
        bad()  { printf '  FAIL  %s\n' "$1"; FAIL=$((FAIL+1)); }
        note() { printf '        %s\n' "$1"; }

        echo "dibs --check on $(hostname -s)"
        echo

        ok "bash ${BASH_VERSION%%(*}, and the login shell parsed the bootstrap"

        missing=""
        for t in flock timeout; do command -v "$t" >/dev/null 2>&1 || missing="$missing $t"; done
        [ -z "$missing" ] && ok "flock and timeout present" || bad "missing, and nothing works without them:$missing"
        rsync_v=$(rsync --version 2>/dev/null | awk 'NR == 1 && $1 == "rsync" {print $3}')
        case "$rsync_v" in
            [3-9]*) ok "rsync $rsync_v, so a tree can be sent here" ;;
            *) bad "no rsync 3, so a tree sent from another computer cannot arrive"
               note "openrsync, which macOS ships as rsync, takes too few of its options." ;;
        esac
        if command -v setpriv >/dev/null 2>&1; then ok "setpriv present, so a job dies with its caller"
        else warn "no setpriv: a job whose caller is killed outright can outlive it"; fi

        # Without it a job whose work runs in short-lived children reads as idle and gets
        # reported stuck.
        if counts_reaped_children; then
            ok "the CPU of reaped children is counted, so idle detection sees a job's whole tree"
        else
            warn "the CPU of reaped children is not counted here: a working job may be misreported as idle"
        fi

        case "$LOCK_SCOPE" in
            shared) ok "lock directory is shared: $DIR" ;;
            explicit) warn "lock directory set explicitly: $DIR (only what set it will agree)" ;;
            *)
                warn "lock directory is keyed to this uid: $DIR"
                note "Anyone logging in as a different user takes a different lock, and both"
                note "are told the machine is idle. Fine if everyone shares this account, and"
                note "a wrong answer with nothing to notice it by if they do not."
                note ""
                shared_lock_howto ;;
        esac

        scratch=${DIBS_SCRATCH:-$HOME/.cache/dibs}
        fstype=$(df -PT "$scratch" 2>/dev/null | awk 'NR==2{print $2}')
        avail=$(df -Ph "$scratch" 2>/dev/null | awk 'NR==2{print $4}')
        if [ ! -w "$scratch" ]; then bad "scratch $scratch is not writable"
        elif [ "$fstype" = tmpfs ]; then
            bad "scratch $scratch is tmpfs, which is RAM and usually under a quota"
            note "One build tree there fills it for every user, and a full tmpfs breaks"
            note "every command including the ones for finding out why. Set DIBS_SCRATCH."
        else ok "scratch $scratch on $fstype, $avail free"; fi

        tdir=$scratch/target
        if [ -d "$tdir" ] && [ -w "$tdir" ]; then
            probe=$tdir/.dibs-reflink-check
            if echo x > "$probe" && cp --reflink=always "$probe" "$probe.copy" 2>/dev/null; then
                tfs=$(df -PT "$tdir/" 2>/dev/null | awk 'NR==2{print $2}')
                listed=$(timeout 60 du -sh "$tdir/" 2>/dev/null | cut -f1)
                ok "target directories on $tfs with reflinks: a new tree starts from a copy of its repo's latest"
                note "$(ls "$tdir" | wc -l) of them list ${listed:-more than 60s of du}, counting shared blocks once per copy."
                [ "$tfs" = xfs ] && note "df is no better a measure: XFS reserves about 2% of the disk up front for reflinks."
            else
                warn "no reflinks where target directories live, so every new tree builds from nothing"
                note "XFS or btrfs under $tdir lets a new tree start from a sibling's build for free."
            fi
            rm -f "$probe" "$probe.copy"
        fi

        case "$HIST" in
            /var/lib/dibs/*) ok "history and log shared: $(dirname "$HIST")" ;;
            *) warn "history is per-user: $HIST"
               note "Estimates are built only from your own runs, and the log shows only"
               note "your own jobs. Shared, as root:  install -d -m 2775 -g dibs /var/lib/dibs" ;;
        esac

        echo
        # Which repos this machine can prepare a worktree from. A machine with no clone of a
        # repo is dropped from that repo's routing entirely, so without this the loss is
        # invisible: the machine looks healthy, and simply never gets that work.
        repos=$(for d in "$HOME/prog"/*; do
                    [ -d "$d/.git" ] || [ -f "$d/.git" ] || continue
                    printf '%s ' "$(basename "$d")"
                done)
        if [ -n "$repos" ]; then
            echo "  repos it can build:  $repos"
        else
            warn "no clones under ~/prog, so no recipe can be run here at all"
            note "It is dropped from routing for every repo until one is cloned. Anything"
            note "this machine should build needs a clone at ~/prog/<repo> first."
        fi
        echo
        echo "  devices"
        cores=$(nproc 2>/dev/null)
        model=$(cpu_model)
        printf '    cpu   %s, %s threads\n' "${model:-unknown}" "${cores:-?}"
        gpu_report

        # Emitted from what was just detected, because a hand-written bus id is how an
        # inventory goes quietly stale. @NAME@ and @SSH@ are the client's to fill: a machine
        # cannot know the alias that reaches it.
        if [ "$LABEL" = check-write ]; then
            echo
            echo "--8<-- dibs inventory --8<--"
            printf '[machine.@NAME@]\n'
            printf 'ssh      = "@SSH@"\n'
            printf 'hostname = "%s"\n' "$(hostname -s 2>/dev/null || hostname)"
            printf 'probed   = "%s"\n' "$(date +%F)"
            # A battery means a laptop, and a laptop throttles, shares one memory pool
            # between CPU and iGPU, and moves. Absence of a GPU says nothing about this:
            # a CPU benchmark box is a machine worth measuring on.
            if has_battery; then
                printf 'measure  = false        # runs on a battery, so it throttles and moves\n'
                printf 'workstation = true      # someone works here; drop this if it is headless\n'
            fi
            printf '\n  [[machine.@NAME@.device]]\n'
            printf '  kind  = "cpu"\n'
            printf '  name  = "%s"\n' "${model:-unknown}"
            printf '  cores = %s\n' "${cores:-0}"
            gpu_entries
            echo "--8<-- end --8<--"
        fi

        echo
        if [ "$FAIL" -gt 0 ]; then
            echo "  $FAIL blocking, $WARN to look at. This machine is not ready."
            exit 1
        elif [ "$WARN" -gt 0 ]; then
            echo "  usable, $WARN thing(s) to look at."
        else
            echo "  ready."
        fi
        exit 0 ;;

    out)
        # A job's stdout is the ssh channel back to whoever started it, and nothing keeps a
        # copy. But agents overwhelmingly redirect into a file, and a redirect is an open
        # descriptor: the kernel will say where it points. So the output is not lost, it is
        # just somewhere nobody thought to look, and the tree walk that already exists for
        # CPU finds it.
        target=${LABEL%%.*} lines=${LABEL##*.} whole=0
        [ "$lines" = whole ] && { whole=1; lines=40; }
        # A job id names a directory, running or finished, so this works after the fact, which
        # is the case the process walk below cannot serve: a job that is gone has no fds.
        case "$target" in
            *-*)
                d=${DIBS_SCRATCH:-$HOME/.cache/dibs}/jobs/$target
                [ -f "$d/log" ] || { echo "no job $target under ${DIBS_SCRATCH:-$HOME/.cache/dibs}/jobs" >&2; exit 1; }
                # The whole log, for the caller to keep, once the job has finished. Capped, since
                # this takes no lock and a benchmark may be running beside it.
                if [ "$whole" = 1 ] && [ -f "$d/meta" ] && [ "$(wc -c < "$d/log")" -le 16777216 ]; then
                    echo DIBS-OUT-HEAD
                    echo "job $target  $(awk -F'\t' '$1=="mode"{m=$2} $1=="label"{l=$2} $1=="exit"{e=$2} $1=="ran"{r=$2} END{print m"  "l"  ran "r"s  exit "e}' "$d/meta")"
                    echo "  $(head -c 200 "$d/cmd" 2>/dev/null | tr '\n' ' ')"
                    echo "  $(hostname -s):$d/log"
                    echo DIBS-OUT-LOG
                    cat "$d/log"
                    exit 0
                fi
                if [ -f "$d/meta" ]; then
                    echo "job $target  $(awk -F'\t' '$1=="mode"{m=$2} $1=="label"{l=$2} $1=="exit"{e=$2} $1=="ran"{r=$2} END{print m"  "l"  ran "r"s  exit "e}' "$d/meta")"
                else
                    echo "job $target  still running"
                fi
                echo "  $(head -c 200 "$d/cmd" 2>/dev/null | tr '\n' ' ')"
                echo "  $d/log  ($(wc -c < "$d/log") bytes, last $lines lines)"
                tail -n "$lines" "$d/log" | sed 's/^/  | /'
                exit 0 ;;
        esac
        found=0
        for f in "$DIR"/holder.*; do
            [ -e "$f" ] || continue
            IFS=$'\t' read -r mode pid start label agent who dev cmd fp < "$f"
            [ "$target" = all ] || [ "$target" = "$pid" ] || continue
            found=1
            echo "$mode  $label  pid $pid  ($(age "$start"))"
            echo "  from $agent"
            # The root's own stdout is the channel it was launched down, not something the
            # job chose. Anything different from that is a redirect the job made itself.
            root=$(fd_path "$pid" 1)
            files=$(fd_targets "$pid" "$root")
            sink=$(ls -d "${DIBS_SCRATCH:-$HOME/.cache/dibs}"/jobs/*-"$pid"/log 2>/dev/null | head -1)
            if [ -z "$files" ] && [ -n "$sink" ]; then
                files=$sink
            fi
            if [ -z "$files" ]; then
                echo "  writing straight back to the agent that started it, so there is no"
                echo "  copy on disk to show. Only a job that redirects into a file, which is"
                echo "  ${C_DIM}cmd > \$DIBS_SCRATCH/x.log 2>&1, can be read from here.$C_OFF"
            else
                for t in $files; do
                    echo
                    echo "  $t  ($(wc -c < "$t" 2>/dev/null || echo 0) bytes, last $lines lines)"
                    tail -n "$lines" "$t" 2>/dev/null | sed 's/^/  | /'
                done
            fi
        done
        [ "$found" = 1 ] || {
            if [ "$target" = all ]; then echo "Nothing is running."
            else echo "Nothing holding the lock with pid $target." >&2; exit 1; fi
        }
        exit 0 ;;
    fetch)
        # No lock, like out, so what it sends is capped.
        d=${DIBS_SCRATCH:-$HOME/.cache/dibs}/jobs/$LABEL
        [ -d "$d" ] || { echo "no job $LABEL under ${DIBS_SCRATCH:-$HOME/.cache/dibs}/jobs" >&2; exit 1; }
        [ -d "$d/artifacts" ] || { echo "job $LABEL kept no files: its recipe names no artifacts, or it wrote none of them" >&2; exit 3; }
        size=$(du -sb "$d/artifacts" | cut -f1)
        if [ "$size" -gt 67108864 ]; then
            echo "job $LABEL kept $size bytes, more than a fetch without a lock takes. Copy them under the shared lock:" >&2
            echo "  dibs --sync -a :$d/artifacts/ ./" >&2
            exit 2
        fi
        echo DIBS-FETCH
        tar -C "$d/artifacts" -cf - . | base64
        exit 0 ;;
    kill|kill-force)
        # Takes no lock. The whole point is to work when the lock is what is broken.
        target=${LABEL%%.*} anyone=""
        [ "${LABEL#*.}" = any ] && anyone=1
        [[ $target =~ ^[0-9]{8}-[0-9]{6}-[0-9]+$ ]] && kill_batch_here "$target" "$anyone"
        found=""
        for f in "$DIR"/holder.* "$DIR"/waiting.*; do
            [ -e "$f" ] || continue
            [ "${f##*.}" = "$target" ] && found=$f
        done
        [ -n "$found" ] || { echo "Nothing holding or queued with pid $target." >&2; show >&2; exit 1; }
        IFS=$'\t' read -r mode pid start label agent who dev cmd fp < "$found"
        if ! still_the_same "$pid" "$found"; then
            rm -f "$found"
            echo "dibs: $mode $label ended without clearing its record, and pid $pid belongs to" >&2
            echo "  something else now. The record is gone and nothing was signalled." >&2
            exit 1
        fi
        # Whose job this is, checked before anything is signalled rather than reported after.
        # Several agents share this machine and read the same --status, so a pid copied out of
        # it belongs to whoever happens to be there; stopping someone else's measurement has
        # to be a thing you meant rather than a thing you did.
        # Keyed on the session rather than on the title it displays under, so a title that goes
        # stale or changes mid-run cannot make an agent a stranger to its own job. A record
        # written before this field existed has no id, and an unknown owner is not grounds to
        # refuse: it would strand exactly the jobs already running during an upgrade.
        if [ -n "$who" ] && [ -n "$AGENT_ID" ] && [ "$who" != "$AGENT_ID" ] && [ -z "$anyone" ]; then
            echo "dibs: $mode $label (pid $pid) belongs to $agent, not to you." >&2
            echo "  It has been running $(age "$start"). If it is stuck and in your way, or you" >&2
            echo "  know it should stop, say so:  dibs --kill $pid --anyone" >&2
            exit 2
        fi
        # Every shell of one account on one laptop has this same id, so a match proves nothing:
        # it is as likely another agent's job as this one's.
        case "$who" in
            shell-*)
                if [ -z "$anyone" ]; then
                    echo "dibs: $mode $label (pid $pid) was started by $who, which names an account, not a" >&2
                    echo "  session, so dibs cannot tell whether it is yours. If you know it is, or that it" >&2
                    echo "  should stop:  dibs --kill $pid --anyone" >&2
                    echo "  Export DIBS_AGENT once per session and your jobs can be told apart." >&2
                    exit 2
                fi ;;
        esac
        # Its descendants, never its process group. The holder is not its group's leader,
        # the pipeline that launched it is, so a group kill signals whatever else happens
        # to share that group rather than the job that was asked for.
        # TERM goes to the holder alone, which stops its own tree before it lets the lock go and
        # logs how it ended; a job killed under it first would end it as a plain failure.
        tree=$pid
        [ "$MODE" = kill-force ] && tree="$pid $(tree_below "$pid")"
        [ "$MODE" = kill-force ] && sig=KILL || sig=TERM
        signalled=""
        # Deepest first, so a parent cannot spawn more work while its children are dying.
        for victim in $(printf '%s\n' $tree | tac); do
            kill -"$sig" "$victim" 2>/dev/null && signalled="$signalled $victim"
        done
        if [ -n "$signalled" ]; then
            CMD_ONE="killed $mode $label (pid $pid) after $(age "$start"): $cmd"
            log_event killed
            echo "Sent SIG$sig to$signalled ($mode $label, held $(age "$start"))."
            echo "It belonged to $agent."
        else
            echo "Could not signal pid $pid; it may already be gone." >&2
        fi
        [ "$MODE" = kill ] && echo "If it survives that, run: dibs --kill $pid --force"
        exit 0 ;;
    release) prune; echo "Pruned dead entries."; reclaim; show
             echo "A live holder is a running command. Stop it with: kill <pid>"; exit 0 ;;
esac

