# A batch belongs to the driver running it. On the computer where that driver runs, it is
# stopped there: nothing more starts, its running steps are stopped, and it prints its summary.
# Anywhere else only the machines can be reached, so each one stops the batch's jobs and refuses
# its later steps, and a driver whose step is refused stops as well.
kill_batch() {   # id
    local id=$1 dir pid owner m tmp status=1
    dir=${XDG_STATE_HOME:-$HOME/.local/state}/dibs/batch/$id
    pid=${id##*-}
    if [ -d "$dir" ] && [ -r "/proc/$pid/cmdline" ] && tr '\0' ' ' < "/proc/$pid/cmdline" | grep -q 'dibs-core batch '; then
        owner=$(cat "$dir/owner" 2>/dev/null)
        if [ -n "$owner" ] && [ "$owner" != "$(agent_id)" ] && [ "$ANYONE" != 1 ]; then
            echo "dibs: batch $id was started by another session. If it should stop:  dibs --kill $id --anyone" >&2
            exit 2
        fi
        : > "$dir/cancel"
        echo "dibs: cancelling batch $id here: nothing more starts, and its running steps are stopped." >&2
        timeout 60 tail --pid="$pid" -f /dev/null 2>/dev/null
        if [ -d "/proc/$pid" ]; then
            kill -TERM "$pid" 2>/dev/null
            echo "dibs: its driver did not stop within a minute, so it was sent SIGTERM; its steps die with it." >&2
        fi
        [ -s "$dir/summary" ] && cat "$dir/summary"
        exit 0
    fi
    [ "$HOSTFROM" = --on ] && [ -n "$MACHINE" ] && set -- "$MACHINE" || set --
    if [ $# -eq 0 ] && [ -f "$MACHINES" ]; then
        while read -r m; do [ -n "$m" ] && set -- "$@" "$m"; done < <(inv_names)
    fi
    if [ $# -eq 0 ]; then
        DIBS_KILL_HERE=1 exec "$0" --kill "$id" $([ "$ANYONE" = 1 ] && echo --anyone) $([ "$FORCE" = 1 ] && echo --force)
    fi
    echo "dibs: batch $id is not driven from this computer, so it is stopped on the machines: $*" >&2
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/dibs-kill.XXXXXX") || exit 1
    for m in "$@"; do
        ( DIBS_KILL_HERE=1 bounded "${DIBS_POLL_TIMEOUT:-20}" "$0" --on "$m" --kill "$id" \
            $([ "$ANYONE" = 1 ] && echo --anyone) $([ "$FORCE" = 1 ] && echo --force) > "$tmp/$m" 2>&1
          echo $? > "$tmp/$m.rc" ) &
    done
    wait
    for m in "$@"; do
        printf '%s\n' "$m"
        if [ -s "$tmp/$m" ]; then sed 's/^/  /' "$tmp/$m"; else echo "  no answer"; fi
        [ "$(cat "$tmp/$m.rc" 2>/dev/null)" = 0 ] && status=0
    done
    rm -rf "$tmp"
    exit "$status"
}
