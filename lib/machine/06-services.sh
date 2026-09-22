ports_listening() {
    if command -v ss >/dev/null 2>&1; then
        ss -ltn 2>/dev/null | awk 'NR > 1 {n = split($4, a, ":"); print a[n]}'
    else
        # 0A is LISTEN. The port is hex, and converting it is bash's job: mawk has no strtonum.
        awk 'NR > 1 && $4 == "0A" {split($2, a, ":"); print a[2]}' /proc/net/tcp /proc/net/tcp6 2>/dev/null |
            while read -r x; do printf '%d\n' "$(( 16#$x ))"; done
    fi
}

# A port two jobs both chose is the collision this exists to prevent, so the reservation is an
# exclusive create under the lock directory, and it names the pid holding it so prune can clear it.
ports_take() {   # 1 when the range has nothing free
    local lo hi used i try p range=${DIBS_PORTS:-20500-20999}
    lo=${range%%-*}; hi=${range##*-}
    used=" $(ports_listening | tr '\n' ' ') "
    for i in "${!PORT_NAME[@]}"; do
        PORT_NUM[$i]=""
        for try in $(seq 200); do
            p=$(( lo + RANDOM % (hi - lo + 1) ))
            case "$used" in *" $p "*) continue ;; esac
            ( set -o noclobber; echo $$ > "$DIR/port.$p" ) 2>/dev/null || continue
            PORT_NUM[$i]=$p
            used="$used$p "
            break
        done
        [ -n "${PORT_NUM[$i]}" ] || return 1
        export "DIBS_PORT_$(printf %s "${PORT_NAME[$i]}" | tr 'a-z' 'A-Z')=${PORT_NUM[$i]}"
    done
}

port_of() {   # name; the port picked for it, or the name back when it is a number already
    local i
    for i in "${!PORT_NAME[@]}"; do
        [ "${PORT_NAME[$i]}" = "$1" ] && { printf '%s' "${PORT_NUM[$i]}"; return 0; }
    done
    printf '%s' "$1"
}

with_lines() {   # pid
    local n p c
    [ -e "$DIR/with.$1" ] || return 0
    while IFS=$'\t' read -r n p c; do
        echo "    ${C_DIM}with $n, pid $p:$C_OFF $C_DIM$c$C_OFF"
    done < "$DIR/with.$1"
}

with_start() {
    local i
    WITH_UP=1
    WITH_STARTED=$(date +%s)
    for i in "${!WITH_NAME[@]}"; do
        WITH_LOG[$i]=/dev/null
        [ -n "$JOBLOG" ] && WITH_LOG[$i]=$JOBDIR/with-${WITH_NAME[$i]}.log
        bash -c "${WITH_CMD[$i]}" 8>&- 9>&- 5<&- < /dev/null > "${WITH_LOG[$i]}" 2>&1 &
        WITH_PID[$i]=$!
        printf '%s\n' "$!" >> "$WORKFILE"
        printf '%s\t%s\t%s\n' "${WITH_NAME[$i]}" "$!" \
            "$(printf %s "${WITH_CMD[$i]}" | tr '\n\t' '  ' | cut -c1-160)" >> "$DIR/with.$$"
    done
}

with_answers() {   # readiness seconds; readiness is empty, tcp:[host:]port, or a command that exits 0
    local a h=127.0.0.1
    case "$1" in
        '') return 0 ;;
        tcp:*) a=${1#tcp:}
               case "$a" in *:*) h=${a%:*}; a=${a##*:} ;; esac
               case "$a" in *[!0-9]*) a=$(port_of "$a") ;; esac
               timeout "$2" bash -c 'exec 3<>"/dev/tcp/$1/$2"' _ "$h" "$a" 8>&- 9>&- 5<&- 2>/dev/null ;;
        *) timeout --kill-after=2 "$2" bash -c "$1" 8>&- 9>&- 5<&- < /dev/null > /dev/null 2>&1 ;;
    esac
}

with_failed() {   # index what consequence; with the end of its log, which is usually where the reason is
    WITH_END[$1]="${WITH_END[$1]:+${WITH_END[$1]}, }$2"
    WITH_BAD[$1]=1
    WITH_FAIL="service ${WITH_NAME[$1]} $2"
    echo "dibs: $WITH_FAIL, so $3." >&2
    if [ -s "${WITH_LOG[$1]}" ]; then
        echo "  The end of its log, ${WITH_LOG[$1]}:" >&2
        tail -n 10 "${WITH_LOG[$1]}" | sed 's/^/    /' >&2
    fi
}

with_ready() {   # 1 when a service is not there to be used
    local i st left deadline=$(( $(date +%s) + READY_WITHIN ))
    for i in "${!WITH_NAME[@]}"; do
        until left=$(( deadline - $(date +%s) )); with_answers "${WITH_READY[$i]}" $(( left > 0 ? left : 1 )); do
            if ! kill -0 "${WITH_PID[$i]}" 2>/dev/null; then
                wait "${WITH_PID[$i]}"; st=$?
                with_failed "$i" "exited $st before it was ready" "the command did not run"
                return 1
            fi
            if [ "$(date +%s)" -ge "$deadline" ]; then
                with_failed "$i" "was not ready within ${READY_WITHIN}s" "the command did not run"
                return 1
            fi
            sleep 0.2
        done
        WITH_END[$i]="ready after $(( $(date +%s) - WITH_STARTED ))s"
    done
}

with_stop() {
    local i alive=()
    for i in "${!WITH_PID[@]}"; do
        kill -0 "${WITH_PID[$i]}" 2>/dev/null || continue
        alive+=("${WITH_PID[$i]}")
        if [ -n "${WITH_BAD[$i]:-}" ]; then WITH_END[$i]="${WITH_END[$i]}, and stopped"
        else WITH_END[$i]="${WITH_END[$i]:+${WITH_END[$i]}, }stopped when the command ended"; fi
    done
    [ "${#alive[@]}" -gt 0 ] && { reap "${alive[@]}"; wait "${alive[@]}" 2>/dev/null; }
    rm -f "$DIR/with.$$"
    WITH_UP=0
}

