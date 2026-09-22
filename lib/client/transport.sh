# The machine's half, sent whole with every call: lib/machine/*.sh in order, as one script.
remote_script() {
    cat "$DIBS_LIB"/machine/*.sh
}

# A target whose scratch directory is full takes out every way of looking at why, so this
# says where to look and how to get working again in the same breath.
no_room() {
    local d=${DIBS_REMOTE_DIR:-'~/.cache/dibs/run'}
    echo "dibs: could not write the job's script to $d on $TARGET." >&2
    echo "  It is full, over quota, or read-only. Nothing can run there until it is not," >&2
    echo "  and that includes every command for looking into it." >&2
    echo "  Look:  DIBS_REMOTE_DIR=/dev/shm dibs --peek 'df -h $d; quota -s'" >&2
    echo "  Work:  DIBS_REMOTE_DIR=/dev/shm dibs <command>" >&2
    echo "  Tell the user. Do not delete anything on a shared machine to make room." >&2
    exit 70
}

unreachable() {
    local why diagnosed=0
    # Ask ssh why before guessing. The failure that brought us here printed its own reason to
    # the caller's terminal and we did not keep it, so it is asked again: one bounded probe on
    # a path that has already failed. Most of what lands here is not the machine being down,
    # and saying it is sends someone to look at a machine that is running fine.
    why=$(bounded "${DIBS_CONNECT_TIMEOUT:-10}" ssh -o BatchMode=yes -o LogLevel=ERROR \
              -o ConnectTimeout="${DIBS_CONNECT_TIMEOUT:-10}" "$HOST" true 2>&1)
    echo "dibs: cannot reach '$HOST' over ssh." >&2
    # Naming the machine is not enough when the caller believes it is talking to a different
    # one. A step in a script that forgot --on went to DIBS_HOST and then reports that machine
    # down, which sends someone to look at a machine nobody was using.
    case "$HOSTFROM" in
        DIBS_HOST) echo "  Nothing on this call named a machine, so it went to DIBS_HOST." >&2
                   echo "  Name one with --on, or export DIBS_ON once to cover every call in a script." >&2 ;;
        "")        [ "$PINNED" = 0 ] && [ -n "$MACHINE" ] &&
                       echo "  Nothing on this call named a machine, and $MACHINE is where it was placed." >&2 ;;
    esac
    case "$why" in
        *"REMOTE HOST IDENTIFICATION HAS CHANGED"*)
            diagnosed=1
            echo "  Its host key is not the one recorded. The machine answered, so it is up;" >&2
            echo "  ssh will not talk to it until you say which key is right." >&2
            echo "  If it was reinstalled or its key regenerated, that is expected:" >&2
            echo "    ssh-keygen -R ${HOST##*@}" >&2
            echo "  then connect once by hand. If it was not, find out why the key changed" >&2
            echo "  before trusting it." >&2 ;;
        *"Host key verification failed"*)
            diagnosed=1
            echo "  Its host key is not in your known_hosts, and dibs connects with BatchMode," >&2
            echo "  which cannot answer the question ssh is asking. The machine is almost" >&2
            echo "  certainly up: this is about trust, not reachability." >&2
            echo "  Record the key by connecting once by hand:  ssh $HOST" >&2 ;;
        *"Permission denied"*)
            diagnosed=1
            echo "  It answered and refused the login, so it is up and this is about keys." >&2
            echo "  Check your key is in that account's authorized_keys." >&2 ;;
        *"Connection refused"*)
            diagnosed=1
            echo "  It answered and nothing is listening on the ssh port, so the machine is up" >&2
            echo "  and sshd is not." >&2 ;;
        *"Could not resolve hostname"*|*"Name or service not known"*|*"nodename nor servname"*)
            diagnosed=1
            echo "  The name '${HOST##*@}' does not resolve from here. A .local name needs mDNS" >&2
            echo "  and the same network; anything else needs DNS." >&2 ;;
        *"Network is unreachable"*|*"No route to host"*)
            diagnosed=1
            echo "  This side has no route to it: the problem is your own network or VPN, not the" >&2
            echo "  machine. Nothing about it can be known from here until that is back." >&2 ;;
    esac
    # Only when ssh's own answer explained nothing, and only for a machine tailscale actually
    # carries: what it says about a machine reached over the LAN is not evidence either way.
    if [ "$diagnosed" = 0 ]; then
        if command -v tailscale >/dev/null 2>&1; then
            state=$(tailscale status 2>&1)
            peer=$(grep -i "[[:space:]]$TARGET[[:space:]]" <<<"$state" | head -1)
            case "$state" in
                *"Logged out"*|*"logged out"*|*"NeedsLogin"*)
                    echo "  Tailscale is logged out, which it is after a reboot." >&2
                    echo "  Log in with: tailscale up" >&2 ;;
                *"stopped"*|*"Stopped"*)
                    echo "  Tailscale is stopped. Start it with: tailscale up" >&2 ;;
                *)  if [ -n "$peer" ] && grep -qi 'offline' <<<"$peer"; then
                        echo "  Tailscale reports $TARGET offline: $peer" >&2
                    elif [ -n "$peer" ]; then
                        echo "  Tailscale reports $TARGET up ($peer), so this is an ssh problem." >&2
                    else
                        echo "  It did not answer: off, asleep, or not on this network. It is not" >&2
                        echo "  a tailnet peer either, so tailscale has nothing to say about it." >&2
                    fi ;;
            esac
        else
            echo "  It did not answer: off, asleep, or not on this network." >&2
        fi
        [ -n "$why" ] && printf '  ssh said: %s\n' "$(printf '%s' "$why" | head -1)" >&2
    fi
    echo "  Do not retry in a loop. Tell the user, and do the work that does not need the machine." >&2
    exit 69
}

# Descriptor 6 writes to a fifo and 7 reads it. Only this process holds 6, so its death is EOF
# on 7 wherever 7 was handed.
# Copies what this process writes to the channel on to the machine, and a bare newline whenever
# a quarter of the lease passes with nothing to copy. It must hold no copy of descriptor 6, or it
# would never read EOF and would go on answering for a caller that is gone.
relay() {
    local line rc ms=$(( LEASE * 250 ))
    [ "$WATCH" = 1 ] && [ "$LEASE" -gt 0 ] || exec cat
    while :; do
        IFS= read -r -t "$(( ms / 1000 )).$(printf %03d $(( ms % 1000 )))" line; rc=$?
        if [ "$rc" = 0 ]; then printf '%s\n' "$line"
        elif [ "$rc" -gt 128 ]; then printf '%s' "${line:-$'\n'}"
        else exit 0
        fi
    done
}

live_channel() {
    local f ok
    f=$(mktemp -u "${TMPDIR:-/tmp}/dibs-live.XXXXXX")
    mkfifo "$f" 2>/dev/null && exec 6<>"$f" && exec 7<"$f"
    ok=$?; rm -f "$f"; return "$ok"
}

hold_tree() {   # pid; it and everything below it
    local c
    printf '%s\n' "$1"
    for c in $(pgrep -P "$1" 2>/dev/null); do hold_tree "$c"; done
}

hold_run() {   # the command that takes the lock
    local mark line holder st status pid p at reach
    # A lock taken here is served from here, whichever machine the call named.
    at=${MACHINE:-${TARGET:-$SELF}} reach=${TARGET:-$SELF}
    [ "$LOCK_AT" = "$(lower "$SELF")" ] && at=$SELF reach=$SELF
    mark=$(mktemp -u "${TMPDIR:-/tmp}/dibs-hold.XXXXXX")
    mkfifo "$mark" 2>/dev/null ||
        { echo "dibs: --hold could not make a fifo in ${TMPDIR:-/tmp}, so nothing ran." >&2; return 2; }
    {
        "$@" > "$mark"
        status=$?
        pid=$(cat "$mark.pid" 2>/dev/null)
        if [ -n "$pid" ] && [ ! -e "$mark.done" ] && kill -0 "$pid" 2>/dev/null; then
            echo "dibs: the lock on $at ended before the command did, so the command was stopped." >&2
            kill -TERM $(hold_tree "$pid") 2>/dev/null
        fi
        rm -f "$mark.pid"
        exit "$status"
    } 6>&- &
    holder=$!
    exec 4<"$mark" 7<&-
    rm -f "$mark"
    while IFS= read -r line <&4; do
        case "$line" in DIBS-HOLDING|"DIBS-HOLDING "*) break ;; esac
        printf '%s\n' "$line"
    done
    case "$line" in
        "DIBS-HOLDING "*)
            # Named where the command here has to reach them, which is the machine, not localhost.
            for p in ${line#DIBS-HOLDING }; do
                HOLD_ENV+=("DIBS_PORT_$(printf %s "${p%%=*}" | tr 'a-z' 'A-Z')=${p#*=}")
                HOLD_ENV+=("DIBS_SERVICE_$(printf %s "${p%%=*}" | tr 'a-z' 'A-Z')=$reach:${p#*=}")
            done ;;
    esac
    if [ "${line%% *}" != DIBS-HOLDING ]; then
        exec 4<&- 6>&-
        wait "$holder"; return
    fi
    echo "dibs: holding the $MODE lock on $at, running here: $COMMAND" >&2
    # Ctrl-C is for the command. Trapped, this carries on and releases the lock with its exit.
    trap : INT
    # The command gets stderr back on 3, while bash's own notice of a stopped command, which
    # would name this function's code, goes nowhere.
    { ( printf '%s\n' "$BASHPID" > "$mark.pid"; exec 2>&3 3>&-
        export DIBS_HOLDING="${DIBS_HOLDING:+$DIBS_HOLDING }$LOCK_AT" ${HOLD_ENV[@]+"${HOLD_ENV[@]}"}
        if [ "${#HOLD_CMD[@]}" -eq 1 ]; then exec bash -c "${HOLD_CMD[0]}"; else exec "${HOLD_CMD[@]}"; fi ) 4<&- 6>&-; } 3>&2 2>/dev/null
    st=$?
    trap - INT
    : > "$mark.done"
    printf 'release %s\n' "$st" >&6
    exec 6>&-
    cat <&4
    exec 4<&-
    wait "$holder"; status=$?
    rm -f "$mark.pid" "$mark.done"
    return "$status"
}
