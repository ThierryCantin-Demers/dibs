# A build that runs for half an hour has gone wrong; a benchmark legitimately might not.
MAXFROM=given
if [ -z "$MAXHOLD" ]; then
    MAXFROM=default
    case "$MODE" in
        bench)     MAXHOLD=7200 ;;
        peek)      MAXHOLD=30 ;;
        rsh|sync)  MAXHOLD=3600 ;;
        *)     MAXHOLD=1800 ;;
    esac
fi

# One argument is a shell string, as ssh itself treats it. Several are quoted individually,
# so an argument containing spaces survives the trip.
if [ "$MODE" != rsh ]; then
    COMMAND=""
    if [ $# -eq 1 ]; then
        COMMAND=$1
    elif [ $# -gt 1 ]; then
        COMMAND=$(printf '%q ' "$@")
    fi
fi
HOLD_CMD=("$@")
# A gc call carries its knobs where a command would go, and the machine turns them into the
# sweep: scratch is its directory, and the clocks are its own environment's to default.
[ "$MODE" = gc ] && COMMAND="${GC_DAYS:-default} $DRY"
[ -n "$LABEL" ] || LABEL=$(basename "$(pwd)")
LABEL=${LABEL//[^A-Za-z0-9._-]/_}
