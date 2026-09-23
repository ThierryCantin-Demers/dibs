# Nothing here has a human behind it. A pager or a credential prompt is a hang.
export GIT_PAGER=cat PAGER=cat GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive

# /tmp here is a tmpfs under a quota, and it is shared: one agent's build tree in it fills
# the quota for everybody, and a full one takes out every command for finding out why. TMPDIR
# comes along so that mktemp and the compilers follow without anyone having to remember.
SCRATCH=${DIBS_SCRATCH:-$HOME/.cache/dibs}
mkdir -p "$SCRATCH/tmp" 2>/dev/null
export DIBS_SCRATCH="$SCRATCH" TMPDIR="$SCRATCH/tmp"

# Pinning the job to one card, so that two runs of one benchmark are two runs on the same
# silicon. By bus id, which is a property of the slot, rather than by index, which is a
# property of the order the driver happened to enumerate in this boot.
#
# CUDA_VISIBLE_DEVICES also narrows the process to that single card, so the runtime's own
# default device is the reserved one and code that never asks for a device still lands on it.
# That matters more than the selection: a benchmark that silently used the default while
# believing it was pinned is the failure this is here to prevent.
if [ -n "$DEV_PCI" ]; then
    export DIBS_DEVICE="$DEV_NAME" DIBS_DEVICE_PCI="$DEV_PCI"
    export CUDA_DEVICE_ORDER=PCI_BUS_ID
    # CUDA_VISIBLE_DEVICES takes an index or a GPU-<uuid>, and never a bus id. Handed one it
    # does not error: it ignores the value and leaves every device visible, so the job runs
    # on whatever is first and looks pinned. The UUID is what the bus id is translated into
    # here, and it is resolved on the machine at run time rather than recorded, so that a
    # card swapped between two slots is followed rather than silently mistaken for its
    # neighbour. Order-independent too, unlike an index.
    case ",$DEV_RT," in
        *,cuda,*)
            _want=${DEV_PCI#*:}      # nvidia-smi pads the domain to eight digits, sysfs to four
            _uuid=$(nvidia-smi --query-gpu=uuid,pci.bus_id --format=csv,noheader 2>/dev/null |
                    awk -F', *' -v b="$_want" 'tolower($2) ~ tolower(b"$") {print $1; exit}')
            # Refused rather than run unpinned. A job that asked for one card and silently got
            # whichever was first produces a number about hardware nobody chose, and nothing
            # downstream can tell that from the number it wanted.
            if [ -z "$_uuid" ]; then
                echo "dibs: asked for $DEV_NAME ($DEV_PCI) and nothing here answers to it." >&2
                echo "  Not running it unpinned: that would measure whichever card is first" >&2
                echo "  and report it under the name of the one you asked for." >&2
                echo "  Check the machine still has that card:  dibs --check $(hostname -s) --write" >&2
                exit 2
            fi
            export CUDA_VISIBLE_DEVICES="$_uuid" ;;
    esac
    # Mesa takes vendor:device, never a bus id. The client refuses to get here when the
    # machine has two cards of one model, so this one is unambiguous.
    case ",$DEV_RT," in
        *,vulkan,*)
            # Two mechanisms because neither covers the whole job on its own.
            #
            # DRI_PRIME takes a PCI address, so it is the one that can tell two cards of one
            # model apart, and it is Mesa's: it moves a RADV device to the front and does
            # nothing for NVIDIA's ICD. MESA_VK_DEVICE_SELECT is a layer above every ICD and
            # so reaches the NVIDIA cards, but it keys on vendor and model, which names both
            # halves of an identical pair. Set together, each covers what the other cannot.
            #
            # Both reorder rather than filter, unlike CUDA_VISIBLE_DEVICES. The default
            # device is the one that was named, which is what almost all code asks for, but a
            # job that enumerates and picks an index itself can still reach another card.
            #
            # Never both at once where the model names two cards. The layer sits above every
            # ICD and reorders after DRI_PRIME has, so it wins, and it picks whichever of the
            # pair it likes: setting the two together sent both halves of an identical pair to
            # the same card while each looked pinned, forced or not.
            #
            # Where the model is unique the layer can go further and hide the rest, which is
            # the guarantee CUDA_VISIBLE_DEVICES gives: a job that enumerates and takes an
            # index of its own then still lands on the card that was named, rather than only a
            # job that asks for the default. An identical pair cannot have it, the selector
            # having no way to name one of the two.
            export DRI_PRIME="pci-$(printf '%s' "$DEV_PCI" | tr ':.' '__')"
            [ -n "$DEV_CHIP" ] && [ "${DEV_TWINS:-1}" = 1 ] &&
                export MESA_VK_DEVICE_SELECT="$DEV_CHIP" \
                       MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE=1 ;;
    esac
fi

# A command sent over ssh runs in a non-login, non-interactive shell, which reads neither
# /etc/profile.d nor the part of ~/.bashrc above the interactive guard. A toolchain installed
# the ordinary way is therefore invisible to every job, and the failure is `cargo: not found`
# on a machine where cargo plainly works the moment you log in and try it by hand.
for d in "$HOME/.cargo/bin" /usr/local/cuda/bin; do
    [ -d "$d" ] || continue
    case ":$PATH:" in *":$d:"*) ;; *) PATH="$d:$PATH" ;; esac
done
export PATH

if [ -n "$BATCH_TAG" ] && [ -e "$DIR/cancelled.${BATCH_TAG%% *}" ]; then
    echo "dibs: batch ${BATCH_TAG%% *} was cancelled with dibs --kill, so this step does not run." >&2
    CMD_ONE="refused, batch cancelled: $(printf %s "$CMD" | tr '\n\t' '  ' | cut -c1-160)"
    log_event refused
    exit 76
fi

if [ "${#WITH_NAME[@]}" -gt 0 ] && ! [ "${BASH_VERSINFO[0]}${BASH_VERSINFO[1]}" -ge 51 ]; then
    echo "dibs: --with needs bash 5.1 on the machine, and $(hostname -s) has $BASH_VERSION. Nothing ran." >&2
    exit 2
fi

if [ "$MODE" = peek ]; then
    PEEK_START=$(date +%s)
    timeout --signal=TERM --kill-after=5 "$MAXHOLD" bash -c "$CMD" < /dev/null
    STATUS=$?
    PEEK_TOOK=$(( $(date +%s) - PEEK_START ))
    CMD_ONE=$(printf %s "$CMD" | tr '\n\t' '  ' | cut -c1-200)
    # Every peek is an event. It is the one thing that deliberately runs beside a
    # measurement, so the log has to be able to say what ran beside which run.
    log_event peek - "$PEEK_TOOK" "$STATUS"
    # A peek is supposed to be free. One that is not has just been charged to whichever
    # benchmark is running, so say so where it will be read, and leave it in the log.
    if [ "$PEEK_TOOK" -ge "${DIBS_PEEK_WARN:-3}" ]; then
        echo "dibs: that --peek took $(dur "$PEEK_TOOK") and ran with no lock, beside" >&2
        echo "  whatever is being measured. Anything that costs time belongs in" >&2
        echo "  'dibs <command>', which takes the shared lock." >&2
        log_event peek-slow - "$PEEK_TOOK" "$STATUS"
    fi
    exit $STATUS
fi

