if [ "$HOLD" = 1 ]; then
    case "$MODE" in
        shared|bench) ;;
        *) echo "dibs: --hold takes a lock for a command run on this computer. It goes alone or with --bench." >&2
           exit 2 ;;
    esac
    [ "$DETACH" = 1 ] && { echo "dibs: --hold runs its command on this computer, and --detach would leave it." >&2; exit 2; }
    # A card is pinned through the environment of the job on the machine, which a command run
    # here never sees, though a service started there with --with does.
    [ -n "$DEVICE" ] && [ "${#WITH_NAME[@]}" = 0 ] &&
        { echo "dibs: --hold runs its command on this computer, so it cannot be pinned to a card there." >&2; exit 2; }
fi
if [ "${#WITH_NAME[@]}" -gt 0 ] || [ "${#PORT_NAME[@]}" -gt 0 ]; then
    case "$MODE" in
        shared|bench) ;;
        *) echo "dibs: --with and --port belong to a call that takes a lock: a run, --bench, or --hold." >&2
           exit 2 ;;
    esac
    # A readiness check on a port nobody picks would wait for something that cannot answer.
    for r in "${WITH_READY[@]}"; do
        case "$r" in
            tcp:*) n=${r#tcp:}; n=${n##*:}
                   case "$n" in
                       *[!0-9]*) case " ${PORT_NAME[*]} " in
                                     *" $n "*) ;;
                                     *) echo "dibs: --ready tcp:$n names no port. Use a number, or --port $n to have one picked." >&2
                                        exit 2 ;;
                                 esac ;;
                   esac ;;
        esac
    done
fi

# --detach moves the caller and nothing else. It used to be a mode, so it and --bench each
# overwrote the other and whichever came last won without a word: --bench --detach measured
# with no lock at all, and --detach --bench held the lock in the foreground of a session that
# was about to close. Detaching is a property of the caller, so it is tracked apart from what
# kind of run it is, and the flags that describe the run are refused here rather than dropped.
# They are not lost: they belong on the dibs call inside, which is where the lock is taken.
if [ "$DETACH" = 1 ]; then
    if [ "$MODE" != shared ]; then
        echo "dibs: --detach starts a caller elsewhere. It does not take the lock itself." >&2
        [ "$MODE" = bench ] &&
            echo "  Alone with it, the measurement would run beside everything else there." >&2
        echo "  Put the run inside it, where the lock and the series both still apply:" >&2
        echo "    dibs --detach 'dibs --bench <command>'" >&2
        exit 2
    fi
    unhonoured=""
    [ -n "$DEVICE" ]      && unhonoured="--device"
    [ "$NEW_SERIES" = 1 ] && unhonoured="--new-series"
    [ -n "$WAIT" ]        && unhonoured="--wait"
    [ -n "$MAXHOLD" ]     && unhonoured="--max"
    [ "${#WITH_NAME[@]}" -gt 0 ] && unhonoured="--with"
    [ "${#PORT_NAME[@]}" -gt 0 ] && unhonoured="--port"
    if [ -n "$unhonoured" ]; then
        echo "dibs: $unhonoured describes a run, and --detach starts a caller." >&2
        echo "  Give it to the dibs call inside:  dibs --detach 'dibs $unhonoured ... <command>'" >&2
        exit 2
    fi
fi
