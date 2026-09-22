CMDB64=$(printf %s "$COMMAND" | base64 | tr -d '\n')

# Who is asking, in terms that lead back to a window. Agents are Claude Code sessions, and
# the desktop keeps each session's title in a file named after it, which is the only name
# for an agent the user ever sees. Falling back to the id still tells two of them apart.
# Only the modes that leave a record need it, and it reads a file to find out.
AGENTB64=""
AGENTIDB64=""
case "$MODE" in
    shared|bench|peek|rsh|kill|kill-force|gc)
        AGENTB64=$(agent_name | base64 | tr -d '\n')
        AGENTIDB64=$(agent_id | base64 | tr -d '\n') ;;
esac
# Set by `dibs batch` and by the recipe layer: which batch this call is a step of, and what is
# still to come, one step per line, so the machine's --status can say how long the batch has left.
BATCHB64=""
case "$MODE" in
    shared|bench|peek|rsh)
        [ -n "${DIBS_BATCH:-}" ] && BATCHB64=$(printf '%s\t%s\n%s\n' "$DIBS_BATCH" "${DIBS_BATCH_STEP:-}" \
            "${DIBS_BATCH_PLAN:-}" | head -n 200 | base64 | tr -d '\n') ;;
esac
# Set by the recipe layer: the fingerprint of the procedure it is about to run, with its values
# already substituted in. The machine files the duration under it as well as under the label, so
# what predicts this job is what this job actually does. Sanitised because it reaches an awk
# field and a tab-separated file, and shortened because it is read by people in the history.
FINGERPRINT=$(printf %s "${DIBS_FINGERPRINT:-}" | tr -cd 'A-Za-z0-9._-' | cut -c1-16)
WITHB64=""
if [ "${#WITH_NAME[@]}" -gt 0 ]; then
    WITHB64=$({ printf '%s\n' "$READY_WITHIN"
                for i in "${!WITH_NAME[@]}"; do
                    printf '%s|%s|%s\n' "${WITH_NAME[$i]}" "$(printf %s "${WITH_READY[$i]}" | base64 | tr -d '\n')" \
                        "$(printf %s "${WITH_CMD[$i]}" | base64 | tr -d '\n')"
                done; } | base64 | tr -d '\n')
fi
# Only a terminal wants a redraw. Piped to a file, --watch is a timestamped log instead.
ISTTY=0; [ -t 1 ] && ISTTY=1

# The lock is held by a job on the machine that only waits, while the command runs here with the
# terminal. Its exit goes down the liveness channel, so a release is not logged as the caller dying,
# and the command is stopped if the lock goes first, since it would then run unlocked.
HOLD_ENV=()

if [ "$(lower "$SELF")" = "$(lower "$TARGET")" ] || [ "${DIBS_LOCAL:-0}" = 1 ]; then
    LOCK_AT=$(lower "$SELF")
else
    LOCK_AT=$(lower "$TARGET")
fi
# Inside a hold, a lock on the same machine queues behind the hold, which only ends when this does.
case "$MODE:: ${DIBS_HOLDING:-} " in
    shared::*" $LOCK_AT "*|bench::*" $LOCK_AT "*|rsh::*" $LOCK_AT "*)
        echo "dibs: this runs inside a --hold of the lock on $LOCK_AT, so it would queue behind that hold and never start." >&2
        echo "  Take the lock once: run this outside the --hold, or peek if it is free to run." >&2
        exit 2 ;;
esac

if [ "$LOCK_AT" = "$(lower "$SELF")" ]; then
    # No channel to watch when this runs on the machine itself, and stdin here belongs to
    # whoever called us: watching it would kill the job the moment they redirect from
    # /dev/null. The caller and the job share a machine, so ordinary process death covers it.
    # bash -s reads the script on its stdin here, so there is no stdin left for a protocol
    # stream to travel on. Nothing is lost: on the machine itself, a copy is a copy.
    case "$MODE" in
        rsh|sync) echo "dibs: --sync reaches the machine from elsewhere. You are on it: use cp." >&2
                  exit 2 ;;
    esac
    CMDFILE=$(mktemp "${TMPDIR:-/tmp}/dibs-cmd.XXXXXX") || exit 70
    printf %s "$CMDB64" > "$CMDFILE"
    if [ "$HOLD" = 1 ]; then
        # A hold is released through the channel, so here too it needs one, and the script
        # comes from a file instead of stdin.
        live_channel || { echo "dibs: --hold could not open a fifo in ${TMPDIR:-/tmp}, so nothing ran." >&2; exit 2; }
        SCRIPT=$(mktemp "${TMPDIR:-/tmp}/dibs-hold-script.XXXXXX") || exit 70
        remote_script > "$SCRIPT"
        hold_here() {
            bash "$SCRIPT" "$MODE" "$LABEL" "$WAIT" "$MAXHOLD" "$VERBOSE" "file:$CMDFILE" 0 "$ISTTY" "$AGENTB64" "$JSON" "$AGENTIDB64" "$DEV_PCI" "$DEV_RT" "$DEVICE" "$DEV_CHIP" "$DEV_TWINS" "$STREAM" "$BATCHB64" 1 0 "$WITHB64" "${PORT_NAME[*]}" "$MAXFROM" "$FINGERPRINT" <&7 7<&-
        }
        hold_run hold_here
    else
        remote_script | bash -s -- "$MODE" "$LABEL" "$WAIT" "$MAXHOLD" "$VERBOSE" "file:$CMDFILE" 1 "$ISTTY" "$AGENTB64" "$JSON" "$AGENTIDB64" "$DEV_PCI" "$DEV_RT" "$DEVICE" "$DEV_CHIP" "$DEV_TWINS" "$STREAM" "$BATCHB64" 0 0 "$WITHB64" "${PORT_NAME[*]}" "$MAXFROM" "$FINGERPRINT"
    fi
    STATUS=$?
    [ "$MODE" = bench ] && [ "$HOLD" = 0 ] && [ "$STATUS" -eq 0 ] && [ "${DIBS_SERIES_CHECK:-1}" = 1 ] && series_record
    series_stayed_put
    exit "$STATUS"
else
    [ -n "$HOST" ] || no_machine
    PAYLOAD=$(remote_script | base64 | tr -d '\n')
    # No TTY on purpose. With one, every tool downstream believes it is interactive: git
    # opens its pager and the job blocks forever on a keystroke nobody will type.
    # The login shell on the far side is whatever that machine has, fish on one of these and
    # bash on another, and it is what runs this line. Keep it to syntax they all read the same
    # way: a pipeline, single quotes, no redirection, no $.
    # Killing the caller has to kill the job, and neither half of that is free. An ssh
    # client whose parent dies keeps running as an orphan with the channel up, so
    # --pdeathsig has the kernel signal it the moment this process dies, SIGKILL included.
    # The remote learns of it through EOF on its stdin, and our end must never reach EOF on
    # its own: a fifo this process holds open read-write delivers neither data nor EOF while
    # we live, and closes with us when we do not.
    #
    # The script and the command go down that same stdin ahead of it, each read by length on
    # the far side. As arguments they are one ssh command string, which the kernel caps at
    # 128KB on both ends, and the script alone is most of that.
    DIE_WITH_ME=""
    [ "${DIBS_NO_PDEATHSIG:-0}" = 1 ] || command -v setpriv >/dev/null 2>&1 \
        && DIE_WITH_ME="setpriv --pdeathsig=TERM"
    [ "${DIBS_NO_PDEATHSIG:-0}" = 1 ] && DIE_WITH_ME=""
    # Without a stdin that never reaches EOF on its own there is nothing to distinguish a
    # dead caller from a live one, and the far side would kill every job the moment it
    # acquired. So a fifo we cannot create disables the watch rather than falling back to
    # whatever stdin we were handed: a job outliving its caller is a nuisance, a job killed
    # while its caller waits for it is the failure this whole thing exists to avoid.
    WATCH=1
    # rsync speaks its protocol over this process's stdin and stdout, so there is nothing
    # spare to hold a liveness channel on. Losing the caller closes the stream, which rsync's
    # far side treats as the failure it is; it does not leave a half-written file behind.
    #
    # Whatever follows the command is read from descriptor 7 and relayed into ssh. The
    # fifo's read end is opened on its own, and neither the relay nor ssh keeps descriptor 6,
    # so this process is the fifo's only writer: its death reaches the far side as EOF even
    # where no parent-death signal took ssh down with it.
    # A hold cannot go without the channel, since releasing it is a message on the channel.
    if [ "$MODE" = rsh ]; then
        exec 7<&0; WATCH=0
    elif [ "${DIBS_NO_LIVE:-0}" = 1 ] && [ "$HOLD" = 0 ]; then
        exec 7<&0; WATCH=0
    elif ! live_channel; then
        [ "$HOLD" = 1 ] && { echo "dibs: --hold could not open a fifo in ${TMPDIR:-/tmp}, so nothing ran." >&2; exit 2; }
        echo "dibs: could not open a liveness fifo; this job will outlive its caller." >&2
        exec 7<&0; WATCH=0
    fi
    [ "${DIBS_NO_WATCHDOG:-0}" = 1 ] && [ "$HOLD" = 0 ] && WATCH=0
    # A laptop that sleeps closes nothing, and the machine would hear of it only when TCP gives up
    # on the connection, hours later. 0 turns it off.
    LEASE=${DIBS_LEASE:-120}
    case "$LEASE" in ''|*[!0-9]*) LEASE=120 ;; esac
    [ "$WATCH" = 1 ] || LEASE=0
    # Not /tmp: that is a tmpfs under a quota there, and when it fills, a tool that needs to
    # write to it to run cannot even be used to look at why. The default is left unexpanded
    # here on purpose, for the remote shell to resolve against its own home.
    REMOTE_DIR=${DIBS_REMOTE_DIR:-'$HOME/.cache/dibs/run'}
    REMOTE_SCRIPT="$REMOTE_DIR/.dibs-payload.$$.$(date +%s).sh"
    # exec so that a script which could not be written never runs half of itself: the write
    # is the only thing between the && and the shell being replaced, and if it fails the line
    # after it is what runs. Both shells that might read this line treat exec and && alike.
    to_machine() {
        $DIE_WITH_ME ssh -o BatchMode=yes -o LogLevel=ERROR \
            -o ConnectTimeout="${DIBS_CONNECT_TIMEOUT:-10}" "$HOST" \
            "mkdir -p $REMOTE_DIR 2>/dev/null; head -c ${#PAYLOAD} | base64 -d > $REMOTE_SCRIPT && \
             exec bash ${DIBS_TRACE:+-x} $REMOTE_SCRIPT '$MODE' '$LABEL' '$WAIT' '$MAXHOLD' '$VERBOSE' 'stdin:${#CMDB64}' \
             '$((1 - WATCH))' '$ISTTY' '$AGENTB64' '$JSON' '$AGENTIDB64' '$DEV_PCI' '$DEV_RT' '$DEVICE' '$DEV_CHIP' '$DEV_TWINS' '$STREAM' '$BATCHB64' '$HOLD' '$LEASE' '$WITHB64' '${PORT_NAME[*]}' '$MAXFROM' '$FINGERPRINT'
exit 70" < <(printf %s "$PAYLOAD" "$CMDB64"; exec <&7 6>&- 7<&-; relay) 6>&- 7<&-
    }
    if [ "$HOLD" = 1 ]; then hold_run to_machine; else to_machine; fi
    STATUS=$?
    [ "$MODE" = bench ] && [ "$HOLD" = 0 ] && [ "$STATUS" -eq 0 ] && [ "${DIBS_SERIES_CHECK:-1}" = 1 ] && series_record
    series_stayed_put
    # 255 is ssh's own failure code, not the command's.
    [ "$STATUS" -eq 255 ] && unreachable
    [ "$STATUS" -eq 70 ] && no_room
    exit "$STATUS"
fi
