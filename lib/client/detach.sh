# The same self-recognition the machines get: pointing this at the box you are already on
# should run there rather than ssh to itself, which is also what makes it testable at all.
queue_run() {  # script-on-stdin, args...
    if [ "${DIBS_QUEUE_LOCAL:-0}" = 1 ] || [ "$(lower "${QUEUE##*@}")" = "$(lower "$SELF")" ]; then
        bash -s -- "$@"
    else
        ssh -o BatchMode=yes -o LogLevel=ERROR \
            -o ConnectTimeout="${DIBS_CONNECT_TIMEOUT:-10}" "$QUEUE" "bash -s -- $*"
    fi
}
