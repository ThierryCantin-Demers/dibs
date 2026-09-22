case "$MODE" in
    registry)
        registry_sync || { echo "dibs: could not fetch $REGISTRY_FROM" >&2; exit 69; }
        echo "$(inv_names | wc -l) machines known, shared list from $REGISTRY_FROM"
        exit 0 ;;
    machines)
        [ -f "$MACHINES" ] || { echo "no inventory at $MACHINES" >&2
                                echo "Write one with:  dibs --check <host> --write" >&2; exit 2; }
        w=0
        while read -r m; do [ "${#m}" -gt "$w" ] && w=${#m}; done < <(inv_names)
        while read -r m; do
            [ -n "$m" ] || continue
            note=""; [ "$(inv "$m" measure)" = false ] && note="  (no measurements)"
            layer=""
            if [ -n "$REGISTRY_FROM" ] || [ -f "$REGISTRY" ]; then
                if [ "$(inv_file "$m")" = "$MACHINES" ]; then layer="  [yours]"; else layer="  [shared]"; fi
            fi
            printf '   %-*s %s%s%s\n' "$w" "$m" "$(inv "$m" ssh)" "$note" "$layer"
            # The aliases --device takes. Without a way to read them the flag cannot be used
            # without opening the inventory by hand, so -v is where they live.
            [ "$VERBOSE" = 1 ] || continue
            while read -r a; do
                [ -n "$a" ] || continue
                printf '     %-28s %-14s %s%s\n' "$a" "$(inv_device "$m" "$a" pci)" \
                    "$(inv_device "$m" "$a" runtimes | tr -d '"[]')" \
                    "$(l=$(inv_device "$m" "$a" link); [ -n "$l" ] && printf '  %s' "$l")"
            done < <(inv_device_names "$m")
        done < <(inv_names)
        grep -q '^[[:space:]]*default[[:space:]]*=' "$MACHINES" "$REGISTRY" 2>/dev/null &&
            echo "  A 'default =' line in the inventory is no longer read: a call names its machine." >&2
        exit 0 ;;
    # Every machine at once, which is also what a status naming no machine means. With routing,
    # a job is somewhere rather than on the machine, and asking each in turn is slow enough that
    # nobody does it.
    status)
        if [ "$ALL" = 1 ] || { [ -z "$HOST" ] && [ "${DIBS_LOCAL:-0}" != 1 ] && [ -f "$MACHINES" ]; }; then
            [ -f "$MACHINES" ] || { echo "no inventory at $MACHINES" >&2; exit 2; }
            jf=""; [ "$JSON" = 1 ] && jf="--json"
            tmp=$(mktemp -d "${TMPDIR:-/tmp}/dibs-all.XXXXXX") || exit 1
            while read -r m; do
                [ -n "$m" ] || continue
                ( bounded "${DIBS_POLL_TIMEOUT:-8}" "$0" --on "$m" --status $jf \
                    > "$tmp/$m" 2>&1 ) &
            done < <(inv_names)
            wait
            first=1
            while read -r m; do
                [ -n "$m" ] || continue
                [ "$first" = 1 ] || echo
                first=0
                printf '%s\n' "$m"
                if [ -s "$tmp/$m" ]; then sed 's/^/  /' "$tmp/$m"
                else echo "  no answer"; fi
            done < <(inv_names)
            rm -rf "$tmp"
            exit 0
        fi ;;
    # The machine this call would use, named. Exit 1 when it goes somewhere with no inventory name
    # to hand back to --on: a DIBS_HOST that is not an entry, or this computer under DIBS_LOCAL.
    # Exit 2 when it goes nowhere until a machine is named, which is what the recipe layer and a
    # batch refuse a measurement over.
    which)
        if [ -z "$MACHINE" ]; then
            if [ -n "$HOST" ]; then
                echo "dibs: this call goes to '$HOST' from DIBS_HOST, which is not in the inventory at $MACHINES." >&2
                echo "  It has no name to give --on. Record it with:  dibs --check $HOST --write" >&2
                exit 1
            elif [ "${DIBS_LOCAL:-0}" = 1 ]; then
                echo "dibs: this call runs on this computer, under DIBS_LOCAL." >&2
                exit 1
            elif [ "$(inv_count)" -gt 1 ]; then
                echo "dibs: no machine: this call names none, and $MACHINES has $(inv_count) to choose from." >&2
            else
                echo "dibs: no machine: no --on, no DIBS_ON, no DIBS_HOST, and no inventory at $MACHINES." >&2
            fi
            exit 2
        fi
        printf '%s\n' "$MACHINE"
        exit 0 ;;
    # Whether a binary built on one machine can run on another. Asked of every machine at
    # once, because the answer is a relation between them and not a property of any one.
    abi)
        if [ "$ALL" = 1 ]; then
            [ -f "$MACHINES" ] || { echo "no inventory at $MACHINES" >&2; exit 2; }
            tmp=$(mktemp -d "${TMPDIR:-/tmp}/dibs-abi.XXXXXX") || exit 1
            while read -r m; do
                [ -n "$m" ] || continue
                ( bounded "${DIBS_POLL_TIMEOUT:-20}" "$0" --on "$m" --abi > "$tmp/$m" 2>/dev/null ) &
            done < <(inv_names)
            wait
            names=""
            for f in "$tmp"/*; do
                [ -s "$f" ] || continue
                m=${f##*/}
                names="$names $m"
                fact() { awk -v k="$1" '$1 == k { print $2; exit }' "$2"; }
                printf '%-18s %-8s v%-3s glibc %-8s rustc %-8s %s\n' \
                    "$m" "$(fact arch "$f")" "$(fact level "$f")" "$(fact glibc "$f")" \
                    "$(fact rustc "$f")" "$(fact nvidia "$f" | sed 's/^$/-/')"
            done
            echo
            for a in $names; do
                for b in $names; do
                    [ "$a" = "$b" ] && continue
                    why="" unknown=""
                    ka=$(fact kernel "$tmp/$a") kb=$(fact kernel "$tmp/$b")
                    la=$(fact level "$tmp/$a")  lb=$(fact level "$tmp/$b")
                    ga=$(fact glibc "$tmp/$a")  gb=$(fact glibc "$tmp/$b")
                    if [ "$ka" != "$kb" ]; then
                        # Not a matter of degree. Mach-O and ELF are different formats and
                        # neither kernel will load the other's, whatever the CPU underneath.
                        why="$ka and $kb do not run each other's binaries at all"
                    elif [ "$(fact arch "$tmp/$a")" != "$(fact arch "$tmp/$b")" ]; then
                        why="different architectures"
                    elif [ -z "$la" ] || [ -z "$lb" ] || [ -z "$ga" ] || [ -z "$gb" ]; then
                        # Silence is not agreement. A machine that could not answer is one this
                        # cannot vouch for, and saying yes on missing facts is the one answer
                        # here with a real cost attached.
                        unknown="one of them reported no instruction set level or no libc"
                    elif [ "$lb" -lt "$la" ]; then
                        why="$b is x86-64-v$lb, below v$la"
                    else
                        lo=$(printf '%s\n%s\n' "$ga" "$gb" | sort -V | head -1)
                        [ "$lo" = "$gb" ] && [ "$lo" != "$ga" ] && why="$b has glibc $lo, older than $a's"
                    fi
                    if [ -n "$unknown" ]; then
                        printf '  %s to %s: unknown, %s\n' "$a" "$b" "$unknown"
                    elif [ -n "$why" ]; then
                        printf '  %s to %s: no, %s\n' "$a" "$b" "$why"
                    else
                        printf '  %s to %s: yes\n' "$a" "$b"
                    fi
                done
            done
            echo
            echo "  Equal levels do not mean equal instruction sets: -C target-cpu=native"
            echo "  reaches past the level it lands in, and a binary built that way can still"
            echo "  fault on a machine this says yes about. Reuse without it is what this covers."
            rm -rf "$tmp"
            exit 0
        fi ;;
    forget)
        [ -f "$MACHINES" ] || { echo "no inventory at $MACHINES" >&2; exit 2; }
        [ -n "$(inv "$FORGET" ssh)" ] || { echo "dibs: no machine named '$FORGET'" >&2; exit 2; }
        # A shared machine is not yours to remove. Overriding it locally is, and that is a
        # different act with a different file, so say which one this would have to be.
        if [ "$(inv_file "$FORGET")" != "$MACHINES" ]; then
            echo "dibs: '$FORGET' comes from the shared registry, not from $MACHINES" >&2
            echo "  Nothing here can remove it. Give it an entry of your own to override it." >&2
            exit 2
        fi
        tmp=$(mktemp "$MACHINES.XXXXXX") || exit 1
        inv_without "$FORGET" > "$tmp" && mv "$tmp" "$MACHINES" || exit 1
        echo "forgot $FORGET"
        exit 0 ;;
esac
