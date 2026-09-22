registry_fresh

# A call names its machine with --on or DIBS_ON, and nothing names one for it. A benchmark's
# series belongs to the machine it ran on, so a forgotten --on that lands somewhere by default
# starts a series nobody meant, with nothing but a line on stderr to show for it. Shared work
# with no machine is placed below instead, and anything else is refused.
#
# DIBS_HOST is the machine of a setup with at most one, where there is nothing to choose; with
# several it chooses nothing, since that is the default this does away with.
if [ "$PINNED" = 0 ]; then
    if [ -n "${DIBS_ON:-}" ]; then
        use_machine "$DIBS_ON"; PINNED=1; HOSTFROM=DIBS_ON
    elif [ "${DIBS_LOCAL:-0}" != 1 ] && [ "$(inv_count)" -gt 1 ]; then
        UNHEEDED=$HOST; HOST=""; TARGET=""
    elif [ -n "$HOST" ] && [ -n "$(inv "$HOST" ssh)" ]; then
        # A name the inventory knows is that machine, reached the way every other command
        # reaches it, which is the rule --check already follows. Taken literally it is a
        # different string for the same box: it may not resolve at all, and where it does it
        # keys a series apart from the same machine reached through --on, so one machine
        # reads as two and the guard refuses a move nobody made.
        use_machine "$HOST"; HOSTFROM=DIBS_HOST
    elif [ -n "$HOST" ] && [ -n "$(inv_by_host "$HOST")" ]; then
        # The ssh string or hostname of a machine the inventory knows. Left unresolved, the
        # machine stays nameless and measure = false is never read, so a benchmark sent this
        # way ran on a machine that had said not to.
        use_machine "$(inv_by_host "$HOST")"; HOSTFROM=DIBS_HOST
    elif [ -z "$HOST" ] && [ "${DIBS_LOCAL:-0}" != 1 ] && [ "$(inv_count)" = 1 ]; then
        use_machine "$(inv_names | grep . | head -1)"; HOSTFROM=only
    fi
fi

# A machine can say it is not worth measuring on, and a laptop sharing one memory pool
# between its CPU and its iGPU is the case this exists for. Left to a rule, the wrong number
# gets produced and looks like every other number.
if [ "$MODE" = bench ] && [ "$MEASURABLE" = 0 ] && [ "$HOLD" = 0 ]; then
    echo "dibs: $MACHINE is marked measure = false, so a benchmark cannot run there." >&2
    echo "  Name one that measures with --on <machine>, or run it shared if it is not a measurement." >&2
    exit 2
fi
