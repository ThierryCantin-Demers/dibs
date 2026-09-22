# Which file defines a machine. The personal file wins outright rather than per key: half an
# entry from each would produce a machine that exists nowhere.
inv_file() {  # machine
    if [ -f "$MACHINES" ] && [ -n "$(inv_from "$MACHINES" "$1" ssh)" ]; then
        printf '%s\n' "$MACHINES"
    elif [ -f "$REGISTRY" ] && [ -n "$(inv_from "$REGISTRY" "$1" ssh)" ]; then
        printf '%s\n' "$REGISTRY"
    fi
}

inv() {  # machine key
    local f
    f=$(inv_file "$1") || return 1
    [ -n "$f" ] || return 1
    inv_from "$f" "$1" "$2"
}

# A machine's own scalars. It stops at the first array-of-tables so that a device's `name`
# cannot answer for the machine's.
inv_from() {  # file machine key
    [ -f "$1" ] || return 1
    awk -v m="machine.$2" -v k="$3" '
        /^[[:space:]]*#/    { next }
        /^[[:space:]]*\[\[/ { in_t = 0; next }
        /^[[:space:]]*\[/   { t = $0; sub(/^[[:space:]]*\[/, "", t); sub(/\].*$/, "", t)
                              in_t = (t == m); next }
        in_t && $0 ~ "^[[:space:]]*" k "[[:space:]]*=" {
            v = $0; sub(/^[^=]*=[[:space:]]*/, "", v)
            if (v ~ /^"/) { sub(/^"/, "", v); sub(/".*$/, "", v) }
            else          { sub(/[[:space:]]*#.*$/, "", v); sub(/[[:space:]]+$/, "", v) }
            print v; exit
        }' "$1"
}

# The device tables. inv_from cannot read these: it walks a machine's own keys and stops at
# every [[, because a device is a table of its own underneath the machine.
inv_device() {  # machine alias key
    local f; f=$(inv_file "$1"); [ -n "$f" ] || return 1
    awk -v m="machine.$1.device" -v want="$2" -v k="$3" '
        # done guards the double print: exit jumps to END, and END calls this again.
        function flush() { if (!done && in_d && a == want && (k in v)) { print v[k]; done = 1; exit }
                           delete v; a = "" }
        /^[[:space:]]*#/    { next }
        /^[[:space:]]*\[\[/ { flush(); t = $0; sub(/^[[:space:]]*\[\[/, "", t)
                              sub(/\]\].*$/, "", t); in_d = (t == m); next }
        /^[[:space:]]*\[/   { flush(); in_d = 0; next }
        in_d && /=/ {
            key = $0; sub(/[[:space:]]*=.*$/, "", key); gsub(/[[:space:]]/, "", key)
            val = $0; sub(/^[^=]*=[[:space:]]*/, "", val)
            if (val ~ /^"/) { sub(/^"/, "", val); sub(/".*$/, "", val) }
            else            { sub(/[[:space:]]*#.*$/, "", val); sub(/[[:space:]]+$/, "", val) }
            v[key] = val; if (key == "alias") a = val
        }
        END { flush() }' "$f"
}

inv_cpu_name() {  # machine; the name of its cpu device, empty if it lists none
    local f; f=$(inv_file "$1"); [ -n "$f" ] || return 0
    awk -v m="machine.$1.device" '
        function flush() { if (!done && in_d && v["kind"] == "cpu" && ("name" in v)) {
                               print v["name"]; done = 1; exit }
                           delete v }
        /^[[:space:]]*#/    { next }
        /^[[:space:]]*\[\[/ { flush(); t = $0; sub(/^[[:space:]]*\[\[/, "", t)
                              sub(/\]\].*$/, "", t); in_d = (t == m); next }
        /^[[:space:]]*\[/   { flush(); in_d = 0; next }
        in_d && /=/ {
            key = $0; sub(/[[:space:]]*=.*$/, "", key); gsub(/[[:space:]]/, "", key)
            val = $0; sub(/^[^=]*=[[:space:]]*/, "", val)
            if (val ~ /^"/) { sub(/^"/, "", val); sub(/".*$/, "", val) }
            else            { sub(/[[:space:]]*#.*$/, "", val); sub(/[[:space:]]+$/, "", val) }
            v[key] = val
        }
        END { flush() }' "$f"
}

inv_device_names() {  # machine; every alias it has
    local f; f=$(inv_file "$1"); [ -n "$f" ] || return 0
    awk -v m="machine.$1.device" '
        /^[[:space:]]*\[\[/ { t = $0; sub(/^[[:space:]]*\[\[/, "", t)
                              sub(/\]\].*$/, "", t); in_d = (t == m); next }
        /^[[:space:]]*\[/   { in_d = 0; next }
        in_d && /^[[:space:]]*alias[[:space:]]*=/ {
            v = $0; sub(/^[^=]*=[[:space:]]*"/, "", v); sub(/".*$/, "", v); print v }' "$f"
}

# How many of a machine's devices carry a given chip id. Two cards of one model cannot be
# told apart by a selector that keys on the model, which is all Vulkan offers.
inv_chip_count() {  # machine chip
    local f n=0 a c; f=$(inv_file "$1"); [ -n "$f" ] || { echo 0; return; }
    while read -r a; do
        [ -n "$a" ] || continue
        c=$(inv_device "$1" "$a" chip)
        [ "$c" = "$2" ] && n=$(( n + 1 ))
    done < <(inv_device_names "$1")
    echo "$n"
}

names_in() {  # file
    [ -f "$1" ] || return 0
    awk '/^[[:space:]]*\[machine\./ {
             t = $0; sub(/^[[:space:]]*\[/, "", t); sub(/\].*$/, "", t)
             sub(/^machine\./, "", t); print t }' "$1"
}

# Both layers, the personal one first so an override is listed once and in its own right.
inv_names() {
    { names_in "$MACHINES"; names_in "$REGISTRY"; } | awk '!seen[$0]++'
}

inv_count() {
    inv_names | grep -c .
}

# The entry for the machine this call is actually going to. --on records its name in MACHINE,
# but DIBS_HOST names a machine by its ssh string and deliberately leaves MACHINE empty, since
# a host string is not an inventory name. Anything that has to read the entry needs the name
# back, and some other machine's entry is not it: that answered every device alias for a card
# in a different box.
inv_name_for_host() {
    local n
    while read -r n; do
        [ -n "$n" ] || continue
        [ "$(inv "$n" ssh)" = "$HOST" ] && { printf '%s\n' "$n"; return 0; }
        [ -n "$TARGET" ] && [ "$(inv "$n" hostname)" = "$TARGET" ] &&
            { printf '%s\n' "$n"; return 0; }
    done < <(inv_names)
    return 1
}

# Everything belonging to one machine, its device tables included, written to stdout minus
# that machine. Both writing and forgetting go through it, because a partial removal leaves a
# device table parented to an entry that is gone.
inv_without() {  # machine
    awk -v n="machine.$1" '
        /^[[:space:]]*\[\[?machine\./ {
            t = $0; sub(/^[[:space:]]*\[+/, "", t); sub(/\]+.*$/, "", t)
            sub(/\.device$/, "", t)
            skip = (t == n)
        }
        !skip { print }
    ' "$MACHINES"
}

# Replaces a machine's whole entry rather than editing fields: a device that was removed has to
# disappear, and a merge that only overwrites what it recognises would leave it there.
inv_write() {  # machine entry
    local n=$1 body=$2 tmp
    mkdir -p "$(dirname "$MACHINES")" || return 1
    touch "$MACHINES" || return 1
    tmp=$(mktemp "$MACHINES.XXXXXX") || return 1
    { inv_without "$n"; printf '\n%s\n' "$body"; } > "$tmp" && mv "$tmp" "$MACHINES"
}

# Fetched with scp because that is the form the source is written in, and because a machine
# that can be reached at all can be reached this way. A failure leaves the cached copy in
# place: a registry that cannot be reached should cost the freshness of a list, never the
# ability to dispatch.
registry_sync() {
    local tmp
    [ -n "$REGISTRY_FROM" ] || { echo "dibs: DIBS_REGISTRY names no source" >&2; return 1; }
    mkdir -p "$(dirname "$REGISTRY")" 2>/dev/null || return 1
    tmp=$(mktemp "$REGISTRY.XXXXXX") || return 1
    if bounded "${DIBS_POLL_TIMEOUT:-10}" scp -q -o BatchMode=yes \
            -o ConnectTimeout="${DIBS_CONNECT_TIMEOUT:-10}" "$REGISTRY_FROM" "$tmp" 2>/dev/null &&
       [ -s "$tmp" ]; then
        mv "$tmp" "$REGISTRY"
        return 0
    fi
    rm -f "$tmp"
    return 1
}

# Refreshed on a clock rather than per call, because an ssh round trip on every invocation
# would be paid by every status, every rank and every job.
registry_fresh() {
    [ -n "$REGISTRY_FROM" ] || return 0
    [ -f "$REGISTRY" ] || { registry_sync || true; return 0; }
    [ -z "$(find "$REGISTRY" -maxdepth 0 -mmin +"${DIBS_REGISTRY_TTL:-1440}" 2>/dev/null)" ] && return 0
    registry_sync || true
}

# The inventory name behind an ssh string or a hostname, or nothing.
inv_by_host() {
    local m h=${1##*@}
    while read -r m; do
        [ -n "$m" ] || continue
        if [ "$(inv "$m" ssh)" = "$1" ] || [ "$(inv "$m" hostname)" = "$h" ]; then
            printf '%s\n' "$m"; return 0
        fi
    done < <(inv_names)
    return 1
}

use_machine() {
    local n=$1 ssh_to host_of
    ssh_to=$(inv "$n" ssh)
    if [ -z "$ssh_to" ]; then
        echo "dibs: no machine named '$n' in $MACHINES" >&2
        if [ -f "$MACHINES" ]; then
            echo "  known:" >&2; inv_names | sed 's/^/    /' >&2
        else
            echo "  There is no inventory yet. Write one with:  dibs --check <host> --write" >&2
        fi
        exit 2
    fi
    MACHINE=$n
    HOST=$ssh_to
    host_of=$(inv "$n" hostname)
    TARGET=${host_of:-${ssh_to##*@}}
    [ "$(inv "$n" measure)" = false ] && MEASURABLE=0
}
