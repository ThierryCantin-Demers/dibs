lock_unaccounted() {   # sets ORPH to the pids, OPENERS to whether anything holds the lock
    local p ino mine
    ORPH="" OPENERS=0
    ino=$(stat -c %i "$DIR/rw" 2>/dev/null) || return 0
    mine=$(pgid_of $$)
    for p in $( { lock_openers "$DIR/rw"; } 8>&- 9>&- ); do
        alive "$p" || continue
        holds_flock "$p" "$ino" || continue
        OPENERS=1
        [ -n "$mine" ] && [ "$(pgid_of "$p")" = "$mine" ] && continue
        [ -e "$DIR/holder.$p" ] || [ -e "$DIR/waiting.$p" ] && continue
        ORPH="$ORPH $p"
    done
}

# An orphan holds the lock through a descriptor, so there is nothing to unlink: it goes when
# the process does, and freeing the machine means ending the process. Two readings a moment
# apart, because a client tests the lock by taking it and lets go at once, and killing a
# passer-by would be a worse failure than the wedge this repairs.
reclaim() {
    local p first="" kept=""
    exec 7>"$DIR/rw"
    if flock -n -x 7 2>/dev/null; then flock -u 7; exec 7>&-; return 0; fi
    exec 7>&-
    lock_unaccounted
    [ -n "$ORPH" ] || return 0
    first=$ORPH
    sleep 0.2
    lock_unaccounted
    for p in $ORPH; do
        case " $first " in *" $p "*) kept="$kept $p" ;; esac
    done
    [ -n "$kept" ] || return 0
    echo "The lock was held by an orphan, which left no record. Reclaiming it:"
    for p in $kept; do
        ps -o pid=,etime=,user=,args= -p "$p" 2>/dev/null | sed 's/^ */  /' | cut -c1-100
    done
    CMD_ONE="reclaimed the lock from orphan pid$kept"
    LABEL=reclaim
    log_event reclaimed
    reap $kept
    for p in $kept; do
        alive "$p" && echo "  pid $p survived; run it again, or kill -9 $p" >&2
    done
}

