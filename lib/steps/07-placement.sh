# Shared work that names no machine is placed rather than refused: ranked by load, with a repo's
# work going to the machine that holds its build cache. --any ranks even where DIBS_HOST names one.
# A detached job is not ranked: it goes to one machine that stays up, and --jobs reads that
# machine, so scattering the jobs would be scattering the record of them too.
if [ "$MODE" = pick ] || { [ "$MODE" = shared ] && [ "$PINNED" = 0 ] &&
                           { [ "$ROUTE" = 1 ] || { [ -z "$HOST" ] && [ "$(inv_count)" -gt 1 ]; }; } &&
                           [ "${DIBS_LOCAL:-0}" != 1 ] && [ "$DETACH" = 0 ] && [ "$HOLD" = 0 ] &&
                           [ "${DIBS_FROM_RUN:-0}" != 1 ]; }; then
    picked=$(pick_machine); ranked=$?
    if [ "$ranked" = 0 ]; then
        [ "$MODE" = pick ] && { printf '%s\n' "$picked"; exit 0; }
        use_machine "$picked"
    elif [ "$MODE" = pick ] || [ -z "$HOST" ]; then
        if [ "$ranked" = 2 ]; then
            echo "dibs: every machine answered, but none has a clone of '$REPO'." >&2
            echo "  A worktree is prepared from \$HOME/prog/$REPO on the machine itself, so one" >&2
            echo "  has to be cloned there before any work on $REPO can be sent to it." >&2
        else
            echo "dibs: no machine in $MACHINES answered, so there was nowhere to place this." >&2
        fi
        exit 69
    fi
fi

# The first read of a finished job's log keeps the whole of it here, and later reads are served
# from that copy, so a log someone needed stays readable when its machine is asleep, gone or
# past its two weeks. A running job's log is not kept: it would stand in for the whole one.
KEPT=${DIBS_KEPT:-${XDG_STATE_HOME:-$HOME/.local/state}/dibs/jobs}

# Detached jobs have a machine of their own, DIBS_QUEUE, and what is already kept here needs none.
if [ -z "$HOST" ] && [ "${DIBS_LOCAL:-0}" != 1 ] && [ "$DETACH" = 0 ]; then
    case "$MODE" in
        jobs|job|cancel) ;;
        kill)  is_batch_id "$KILLPID" || no_machine ;;
        out)   [ -n "$OUTPID" ] && [ -f "$KEPT/$OUTPID/log" ] || no_machine ;;
        fetch) [ -n "$FETCHJOB" ] && [ -d "$KEPT/$FETCHJOB/artifacts" ] || no_machine ;;
        *)     no_machine ;;
    esac
fi
