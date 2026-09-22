# Where a shared job should go. Never a benchmark: duration history and label bindings key on
# the machine, so routing one would file two machines' numbers under a single label, which is
# the wrong conclusion the whole thing exists to prevent. Until bindings exist, a measurement
# goes where it is told.
#
# Ranking decides nothing about correctness. The acquire is still an flock, so a stale reading
# costs a worse queue and never a double booking.
pick_machine() {
    local m f tmp best="" best_score="" score state cores load prefer_state=""
    local cached="" cached_score=0 seat="" answered=0
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/dibs-route.XXXXXX") || return 1
    local down=${DIBS_ROUTE_DOWN:-${XDG_STATE_HOME:-$HOME/.local/state}/dibs/route-down}
    local backoff=${DIBS_ROUTE_BACKOFF:-300} now; now=$(date +%s)
    mkdir -p "$down" 2>/dev/null
    while read -r m; do
        [ -n "$m" ] || continue
        # A machine that gave no answer costs the whole probe timeout on every dispatch until
        # it comes back, and every bare shared call is a dispatch. It is skipped for a while
        # instead, and said so, because a machine dropping out silently degrades this to
        # "whichever one answered", which looks exactly like a working ranking.
        if [ -f "$down/$m" ] && [ $(( now - $(cat "$down/$m" 2>/dev/null || echo 0) )) -lt "$backoff" ]; then
            [ "$VERBOSE" = 1 ] && printf '  %-18s no answer %ss ago, not asked again yet\n' "$m" \
                "$(( now - $(cat "$down/$m") ))" >&2
            continue
        fi
        # Bounded, because ssh can hang past its connect timeout on a machine that answered
        # the handshake and then stopped, and one of those must not stall every dispatch.
        ( bounded "${DIBS_POLL_TIMEOUT:-5}" "$0" --on "$m" --status --json 2>/dev/null \
            > "$tmp/$m" ) &
    done < <(inv_names)
    wait
    for f in "$tmp"/*; do
        [ -e "$f" ] || continue
        m=${f##*/}
        if [ ! -s "$f" ]; then
            [ "$VERBOSE" = 1 ] && printf '  %-18s no answer\n' "$m" >&2
            printf '%s\n' "$now" > "$down/$m" 2>/dev/null
            continue
        fi
        rm -f "$down/$m"
        state=$(sed -n 's/.*"state":"\([^"]*\)".*/\1/p' "$f" | head -1)
        cores=$(sed -n 's/.*"cores":\([0-9]*\).*/\1/p' "$f" | head -1)
        load=$(sed -n 's/.*"load":\([0-9]*\).*/\1/p' "$f" | head -1)
        [ -n "$state" ] || continue
        # A machine with no clone of the repo cannot prepare a worktree from it, so it is
        # dropped rather than ranked last: last still wins when it is the only one that
        # answered, and the job would then fail at prepare having queued for it first. A
        # machine that reports no clones at all is one running a dibs too old to say, and is
        # left in the ranking rather than being read as empty.
        answered=$(( answered + 1 ))
        if [ -n "$REPO" ] && grep -q '"clones":\[' "$f" &&
           ! grep -q "\"clones\":\[[^]]*\"$REPO\"" "$f"; then
            [ "$VERBOSE" = 1 ] && printf '  %-18s no clone of %s\n' "$m" "$REPO" >&2
            continue
        fi
        [ -n "${cores:-}" ] && [ "$cores" -gt 0 ] 2>/dev/null || cores=1
        score=$(( ${load:-0} / cores ))
        # A machine held exclusively cannot start a shared job at all, so it is ranked behind
        # every machine that can, rather than excluded: it may still be the only one up.
        [ "$state" = bench ] && score=$(( score + 1000 ))
        # A machine someone works at costs more than its load says: a build taking every
        # thread costs them their editor. It is a property of the machine, not of whoever
        # dispatched. A headless box that happens to be running the agents is not a
        # workstation, and discounting it would waste the best-placed build node there is.
        [ "$(inv "$m" workstation)" = true ] && score=$(( score + ${DIBS_SELF_PENALTY:-25} ))
        # A repo nobody has built yet should have its cache land where it can be measured. A
        # machine that refuses benchmarks is a machine no benchmark can follow the cache to,
        # and the first bench would then compile inside its own exclusive lock. This only
        # decides the bootstrap: once a machine holds the cache it is chosen for holding it,
        # whatever it scores here.
        [ "$(inv "$m" measure)" = false ] && score=$(( score + 500 ))
        # Ties broken at random rather than by name. Every client reading the same snapshot and
        # resolving a tie the same way is how they all pick the same machine.
        [ "$m" = "$PREFER" ] && prefer_state=$state
        [ "$(inv "$m" workstation)" = true ] && seat=", someone works here" || seat=""
        # An observed cache is ground truth; the recorded preference is only a memo of it.
        if [ -n "$REPO" ] && grep -q "\"caches\":\[[^]]*\"$REPO\"" "$f"; then
            [ -z "$cached" ] || [ "$score" -lt "$cached_score" ] && { cached=$m; cached_score=$score; }
        fi
        [ "$VERBOSE" = 1 ] && printf '  %-18s %s%% busy, %s%s%s\n' "$m" "$score" "$state" \
            "$seat" "$([ "$m" = "$PREFER" ] && echo ", holds the cache")" >&2
        if [ -z "$best_score" ] || [ "$score" -lt "$best_score" ] ||
           { [ "$score" = "$best_score" ] && [ $(( RANDOM % 2 )) = 1 ]; }; then
            best=$m; best_score=$score
        fi
    done
    rm -rf "$tmp"
    # A machine holding this repo's build cache wins even while it is busy, and being busy is
    # not a reason to look elsewhere. Nothing built on another machine can be used here: there
    # is no way to move artifacts between machines, so a build placed away from the cache is
    # work thrown away, and the benchmark that follows it still finds nothing and compiles
    # inside its own exclusive lock. Queueing is slower for one job; the alternative is
    # wasted. Only a machine that did not answer at all is given up on.
    if [ -n "$PREFER" ] && [ -n "$prefer_state" ]; then
        printf '%s\n' "$PREFER"
        return 0
    fi
    if [ -n "$cached" ]; then
        printf '%s\n' "$cached"
        return 0
    fi
    # Two different failures, and telling a caller the machines are down when they all
    # answered sends them to look at the network for a missing clone.
    if [ -z "$best" ]; then
        [ "$answered" -gt 0 ] && return 2
        return 1
    fi
    printf '%s\n' "$best"
}

no_machine() {
    local names
    names=$(inv_names | grep . | paste -sd, - | sed 's/,/, /g')
    if [ -z "$names" ]; then
        echo "dibs: no machine. Record one with:  dibs --check <host> --write" >&2
        echo "  Or, for a single machine and no inventory, set DIBS_HOST to it." >&2
    else
        echo "dibs: this call names no machine. Name one of: $names" >&2
        echo "  with --on <machine>, or export DIBS_ON=<machine> to cover every call that follows." >&2
        [ "$MODE" = bench ] &&
            echo "  A measurement is never placed for you: its series belongs to the machine it ran on." >&2
        [ -n "$UNHEEDED" ] &&
            echo "  DIBS_HOST=$UNHEEDED does not choose one when the inventory has several." >&2
    fi
    exit 2
}
