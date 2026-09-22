# A device is named from the inventory rather than given as an index, because what makes two
# runs comparable is that they name the same physical card, and an index does not: it
# renumbers when a card is added, when the driver reorders, and between boots.
if [ -n "$DEVICE" ]; then
    dev_m=${MACHINE:-$(inv_name_for_host)}
    [ -n "$dev_m" ] || {
        echo "dibs: --device names a card from a machine's entry, and ${HOST:-this call} has none." >&2
        echo "  Record one with:  dibs --check ${HOST:-<host>} --write" >&2; exit 2; }
    DEV_PCI=$(inv_device "$dev_m" "$DEVICE" pci)
    DEV_CHIP=$(inv_device "$dev_m" "$DEVICE" chip)
    DEV_RT=$(inv_device "$dev_m" "$DEVICE" runtimes | tr -d '"[] ')
    DEV_TWINS=$(inv_chip_count "$dev_m" "$DEV_CHIP")
    # A CPU is a device a run can name and not one it can choose between: there is one, it
    # needs no pinning, and it has no PCI address to be found by. Refusing it for that left a
    # CPU benchmark unable to say what it ran on, and told besides that it had named none of
    # the GPUs, which is true and is advice about a run that was never going to use one.
    CPU_NAME=""
    [ -z "$DEV_PCI" ] && [ "$DEVICE" = cpu ] && CPU_NAME=$(inv_cpu_name "$dev_m")
    if [ -z "$DEV_PCI" ] && [ -z "$CPU_NAME" ]; then
        echo "dibs: $dev_m has no device called '$DEVICE'." >&2
        if [ -n "$(inv_device_names "$dev_m")" ]; then
            echo "  it has:" >&2; inv_device_names "$dev_m" | sed 's/^/    /' >&2
            [ -n "$(inv_cpu_name "$dev_m")" ] && echo "    cpu" >&2
        elif [ -n "$(inv_cpu_name "$dev_m")" ]; then
            echo "  it has:" >&2; echo "    cpu" >&2
        else
            echo "  Its entry lists no devices. Re-probe it:  dibs --check $dev_m --write" >&2
        fi
        exit 2
    fi
fi

# A benchmark that named no card on a machine that has several is not reproducible, and
# nothing downstream can tell its number from one that was pinned. dibs cannot record which
# card the process chose, because that choice happens inside the runtime where it cannot see,
# so the only honest thing it can do is say the number is unpinned before it is believed.
if [ "$MODE" = bench ] && [ -z "$DEVICE" ] && [ "$HOLD" = 0 ] && [ "$PREFLIGHT" = 0 ] && [ "${DIBS_UNPINNED_QUIET:-0}" != 1 ]; then
    bench_m=${MACHINE:-$(inv_name_for_host)}
    if [ -n "$bench_m" ]; then
        gpus=$(inv_device_names "$bench_m" | grep -c '^gpu:') || gpus=0
        if [ "${gpus:-0}" -gt 1 ]; then
            echo "dibs: $bench_m has $gpus GPUs and this benchmark named none of them." >&2
            echo "  It will run on whichever the runtime picks, which is not something you" >&2
            echo "  can repeat on purpose, and the number will look like any other." >&2
            echo "  Name one with --device. The aliases are in:  dibs --machines -v" >&2
        fi
    fi
fi
