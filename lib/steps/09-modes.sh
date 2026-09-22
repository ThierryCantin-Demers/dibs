# --write wants both halves of the check: the report to show and the entry to record. So it
# runs the check itself and reads its output, which also keeps every bit of host resolution
# in one place. The recursion terminates because the inner call has DIBS_EMIT_ENTRY set.
if [ "$MODE" = check ] && [ "$WRITE" = 1 ] && [ "${DIBS_EMIT_ENTRY:-0}" != 1 ]; then
    [ -n "${MACHINE:-$TARGET}" ] ||
        { echo "dibs --check --write: name the machine, with --on or a host" >&2; exit 2; }
    out=$(DIBS_EMIT_ENTRY=1 "$0" "${ORIG_ARGS[@]}"); status=$?
    printf '%s\n' "$out" | sed '/^--8<-- dibs inventory/,/^--8<-- end/d'
    entry=$(printf '%s\n' "$out" | sed -n '/^--8<-- dibs inventory/,/^--8<-- end/p' | sed '1d;$d')
    [ -n "$entry" ] || { echo "dibs: the machine reported nothing to record." >&2; exit "$status"; }
    # A machine nobody has used is named by an ssh string, and `user@host.local` is a poor
    # key for something you then reach with --on. The probe reports the machine's own short
    # hostname, so that names it and the ssh string is left to do the reaching.
    name=$MACHINE
    if [ -z "$name" ]; then
        name=$(printf '%s\n' "$entry" | sed -n 's/^hostname *= *"\(.*\)"/\1/p' | head -1)
        name=$(printf '%s' "${name:-$TARGET}" | tr -cd 'A-Za-z0-9._-')
        [ -n "$name" ] || { echo "dibs: the machine reported no usable name; give it one with --on" >&2; exit 1; }
    fi
    entry=${entry//@NAME@/$name}
    entry=${entry//@SSH@/$HOST}
    inv_write "$name" "$entry" || { echo "dibs: could not write $MACHINES" >&2; exit 1; }
    echo "  recorded as [machine.$name] in $MACHINES"
    exit "$status"
fi

case "$MODE" in
    kill)
        [ -n "$KILLPID" ] || { echo "--kill needs the pid from --status, or a batch id" >&2; exit 2; }
        if is_batch_id "$KILLPID" && [ "${DIBS_KILL_HERE:-0}" != 1 ]; then
            kill_batch "$KILLPID"
        fi
        is_batch_id "$KILLPID" ||
            case "$KILLPID" in (*[!0-9]*) echo "not a pid or a batch id: $KILLPID" >&2; exit 2 ;; esac
        [ "$FORCE" = 1 ] && MODE=kill-force
        LABEL=$KILLPID
        [ "$ANYONE" = 1 ] && LABEL=$KILLPID.any ;;
    log)
        [ $# -eq 0 ] || { echo "--log takes only a line count" >&2; exit 2; }
        LABEL=$LOG_N ;;
    check)
        [ $# -eq 0 ] || { echo "--check takes only a host" >&2; exit 2; }
        LABEL=check
        [ "${DIBS_EMIT_ENTRY:-0}" = 1 ] && LABEL=check-write ;;
    fetch)
        [ $# -eq 0 ] || { echo "--fetch takes a job id and, optionally, a directory to copy into" >&2; exit 2; }
        [ -n "$FETCHJOB" ] && [ -z "${FETCHJOB//[0-9-]/}" ] ||
            { echo "--fetch needs a job id, the one after 'job' in the trailer" >&2; exit 2; }
        LABEL=$FETCHJOB ;;
    out)
        [ $# -eq 0 ] || { echo "--out takes only a pid or a job id" >&2; exit 2; }
        # A dot because the label is sanitized down to [A-Za-z0-9._-] before it ships.
        LABEL="${OUTPID:-all}.$OUT_N" ;;
    watch)
        [ $# -eq 0 ] || { echo "--watch takes only an interval in seconds" >&2; exit 2; }
        # A reader is not a person, so the floor that protects a benchmark from a human
        # redrawing too fast does not apply. The tick still costs what a redraw costs.
        [ "$JSON" = 1 ] && [ "$WATCH_N" -lt 1 ] && WATCH_N=1
        # Every tick is charged to whoever is being measured, and a human reading a queue
        # does not need a redraw a second.
        [ "$WATCH_N" -ge 2 ] || [ "$JSON" = 1 ] || { echo "--watch: 2 seconds is the floor. Anything faster costs the benchmark more than it tells you." >&2; exit 2; }
        LABEL=$WATCH_N ;;
    sync)
        # rsync's own options pass through untouched. One side carries a leading colon for the
        # machine, the way rsync itself spells a remote path, so both directions are one mode.
        [ $# -ge 2 ] || { echo "--sync takes rsync options, a source and a destination" >&2; usage 2; }
        SYNC=()
        for a in "$@"; do
            case "$a" in
                --on|--label|--bench|--device|--wait|--max|--new-series)
                    # Everything after --sync belongs to rsync, which would hand a dibs flag
                    # to the far side as a path or reject it as an option of its own.
                    echo "dibs: $a is a dibs flag, and after --sync everything is rsync's." >&2
                    echo "  Put it before:  dibs $a ... --sync <opts> <src> <dst>" >&2
                    exit 2 ;;
                :*) SYNC+=("$HOST:${a#:}") ;;
                *)  SYNC+=("$a") ;;
            esac
        done
        # -a and -t carry mtimes across, so a source tree synced over an older copy of itself
        # looks unchanged to cargo, and the build after it compiles nothing. One line, before
        # the bytes move; the copy itself is still what was asked for.
        case " ${SYNC[*]} " in
            *" --no-times "*|*" --no-t "*) ;;
            *" -a "*|*" -t "*|*" -"[a-zA-Z]*"a"*" "*|*" --archive "*|*" --times "*)
                case "${SYNC[-1]}" in
                    "$HOST:"*) echo "dibs: --sync is preserving mtimes into the machine. A build there may then compile nothing:" >&2
                               echo "  for a source tree use --checksum --no-times instead of -a." >&2 ;;
                esac ;;
        esac
        printf '%s\n' "${SYNC[@]}" | grep -q "^$HOST:" || {
            echo "--sync: mark the machine's side with a leading colon, as in :~/.cache/dibs/x" >&2
            exit 2
        }
        command -v rsync >/dev/null 2>&1 || { echo "--sync needs rsync on both machines" >&2; exit 2; }
        # Without it a destination whose parent does not exist yet fails. Only offered where this
        # rsync knows the flag, since an older one rejects the whole transfer over it.
        case " ${SYNC[*]} " in
            *" --mkpath "*|*" --no-mkpath "*) ;;
            *) case "$(rsync --help 2>/dev/null)" in *--mkpath*) SYNC=(--mkpath "${SYNC[@]}") ;; esac ;;
        esac
        # On the machine itself there is no transport, because both sides are ordinary paths.
        # Going through one anyway put a second dibs in the middle, and that one refuses to be
        # a transport to where it already is, so the transfer failed after announcing itself.
        # It reads as a refusal to copy, which it is not: a local copy is the same work, and it
        # goes through the same lock below because a copy competes for bandwidth like anything
        # else. The caller that cannot route around this is a program, which cannot take the
        # advice to use cp.
        if [ "$(lower "$SELF")" = "$(lower "$TARGET")" ] || [ "${DIBS_LOCAL:-0}" = 1 ]; then
            HERE=()
            for a in "${SYNC[@]}"; do HERE+=("${a#"$HOST":}"); done
            # An ordinary shared job from here on, which is what a local copy is.
            # Handed on as the positional command, because the general path below rebuilds
            # COMMAND from the arguments and would otherwise overwrite this with rsync's flags.
            BEFORE=$(sync_before) || exit 2
            set -- "${BEFORE:+$BEFORE$'\n'}rsync$(printf ' %q' "${HERE[@]}")"
            MODE=shared
            LABEL=${LABEL:-sync}
        else
        # rsync runs here and reaches the machine through this same script, which is what takes
        # the lock. One transfer, one holder, whichever direction it goes in.
        SELFPATH=$(readlink -f "$0" 2>/dev/null || echo "$0")
        # Through bash rather than exec'ing the file: rsync runs the transport itself, and
        # whether this copy happens to carry an exec bit is not something to depend on.
        #
        # The transport is a second dibs that parses its own arguments and has never heard of
        # the --on this one was given. The resolved machine is handed to it through the
        # environment, which a forked child inherits and an argument would not survive, since
        # --rsh takes rsync's own host and command after it.
        export DIBS_HOST="$HOST" DIBS_HOSTNAME="$TARGET" DIBS_SYNC_LABEL="$LABEL"
        [ -n "$MACHINE" ] && export DIBS_ON="$MACHINE"
        # Said before the bytes move, because a transfer to the wrong machine is not something
        # you find out about afterwards: it succeeds, exit 0, and the files are somewhere
        # nobody will look. One line on stderr, so it cannot get into rsync's own output.
        printf 'dibs: syncing with %s%s\n' "$HOST" \
            "$([ -n "$MACHINE" ] && [ "$MACHINE" != "$HOST" ] && printf ' (%s)' "$MACHINE")" >&2
        exec rsync -e "bash $SELFPATH --rsh" "${SYNC[@]}"
        fi
        ;;
    rsh)
        # What rsync invokes as its transport: a host it chose and the command to run there.
        # Joined with spaces and not requoted, exactly as ssh would, since rsync has already
        # quoted what needs it and expects the far shell to expand the rest.
        [ $# -ge 2 ] || { echo "--rsh is rsync's transport, not for calling directly" >&2; exit 2; }
        [ "$1" = "-l" ] && shift 2 # a user, when rsync was given one; the ssh config owns that
        shift                      # the host, which we already know
        BEFORE=$(sync_before) || exit 2
        COMMAND=${BEFORE:+$BEFORE$'\n'}$*
        # The label was given to the dibs that started rsync, not to this one, and it
        # arrives the same way the host does. Without it every transfer ever made is filed
        # under one name, which is why nothing about copying could be estimated.
        [ -n "$LABEL" ] || LABEL=${DIBS_SYNC_LABEL:-sync}
        ;;
    status|release|abi|jobs) [ $# -eq 0 ] || { echo "$MODE takes no command" >&2; exit 2; } ;;
    gc)
        [ $# -eq 0 ] || { echo "--gc takes no command: it sweeps what is under the machine's scratch" >&2; exit 2; }
        case "${GC_DAYS:-0}" in (*[!0-9]*) echo "--days takes a number of days" >&2; exit 2 ;; esac
        # The listing is the whole output, and a digest would cut out the middle, which is
        # where the space went.
        STREAM=1
        [ -n "$LABEL" ] || LABEL=dibs-gc ;;
    job|cancel)
        [ -n "$JOBID" ] || { echo "--$MODE needs an id from --jobs" >&2; exit 2; }
        [ $# -eq 0 ] || { echo "--$MODE takes only an id" >&2; exit 2; }
        # A job id carries its date, so a bare number is a pid, and a pid means a holder on
        # the machine rather than a job on the queue. Two namespaces, and the refusal that
        # reached this first was about the queue, which sent people looking at the wrong one.
        case "$JOBID" in
            *[!0-9]*) ;;
            *) echo "dibs: $JOBID is a pid, not a job id from --jobs." >&2
               [ "$MODE" = cancel ] &&
                   echo "  A holder is not a detached job. Stop it with:  dibs --kill $JOBID" >&2
               [ "$MODE" = job ] &&
                   echo "  A holder is not a detached job. dibs --status shows it." >&2
               exit 2 ;;
        esac
        LABEL=$JOBID ;;
    *)              [ $# -gt 0 ] || { echo "no command given" >&2; usage 2; } ;;
esac

if [ "$MODE" != gc ]; then
    [ "$DRY" = 1 ] && { echo "dibs: --dry-run belongs to --gc. A recipe run takes it after its verb." >&2; exit 2; }
    [ -n "$GC_DAYS" ] && { echo "dibs: --days belongs to --gc." >&2; exit 2; }
fi
