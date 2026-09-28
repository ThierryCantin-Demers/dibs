# Linux, read from /proc and /sys.
if [ "$PLATFORM" = linux ]; then

# /proc rather than kill -0: signalling another user's process fails with EPERM, which would read a
# live holder on a machine with more than one account as gone.
alive() {   # pid
    [ -d "/proc/$1" ]
}

proc_state() {   # pid; the one-letter state, empty once it has gone
    sed 's/.*) //; s/ .*//' "/proc/$1/stat" 2>/dev/null
}

started_at() {   # pid; when it started, in seconds since the epoch
    local rest
    rest=$(sed 's/.*) //' "/proc/$1/stat" 2>/dev/null) || return 1
    [ -n "$rest" ] || return 1
    [ -n "${BTIME:-}" ] || BTIME=$(awk '/^btime/{print $2}' /proc/stat 2>/dev/null)
    set -- $rest
    printf '%s' $(( BTIME + ${20} / CLK ))
}

pgid_of() {   # pid; prints its process group, empty if it has gone
    local st
    st=$(< "/proc/$1/stat") 2>/dev/null || return 1
    st=${st#*) }             # the command is parenthesised and may contain spaces
    set -- $st
    printf '%s\n' "$3"
}

holds_flock() {   # pid inode; whether one of its descriptors is the one carrying the lock
    local f k rest
    for f in /proc/"$1"/fdinfo/*; do
        # A descriptor closed between the glob and the read is gone, not an error worth
        # printing: stderr goes first, or bash reports the open before the redirect takes.
        while read -r k rest; do
            [ "$k" = "lock:" ] || continue
            case "$rest" in *":$2 "*) return 0 ;; esac
        done 2>/dev/null < "$f"
    done
    return 1
}

lock_openers() {   # file; the pids with it open
    fuser "$1" 2>/dev/null | tr -s ' ' '\n' | grep -E '^[0-9]+$'
}

children_of() {   # pid; its children, one per line
    local f k
    for f in /proc/$1/task/*/children; do
        [ -r "$f" ] || continue
        k=""; read -r k 2>/dev/null < "$f"
        printf '%s\n' $k
    done
}

fd_path() {   # pid fd; what the descriptor points at, a path or something that is not one
    readlink "/proc/$1/fd/$2" 2>/dev/null
}

# CPU seconds burned by a holder and everything under it, including children it has
# already reaped. That last part is the whole difficulty: a supervisor that spawns a
# benchmark, waits for it, and spawns the next one owns almost no CPU itself at any given
# instant, and ps TIME only reports the living. Reading utime+stime+cutime+cstime out of
# /proc counts the work its finished children did, which is where a sweep's time lives.
#
# The kernel lists a process's children, so the walk descends the holder's own tree instead
# of reading every process on the machine, and every step of it is a shell builtin. That is
# the difference between a --watch tick costing a benchmark nothing and costing it a
# machine-wide /proc scan and thirty forks, five times a minute.
HAVE_CHILDREN=0
[ "${DIBS_NO_CHILDREN:-0}" = 1 ] || { [ -r "/proc/$$/task/$$/children" ] && HAVE_CHILDREN=1; }

tree_cpu_ticks() {   # root pid; sets CPU_TICKS for it and everything under it
    [ "$HAVE_CHILDREN" = 1 ] || { CPU_TICKS=$(cpu_ticks_scan "$1"); return; }
    local -a q=("$1") a
    local i=0 pid line rest kids k f total=0
    while [ "$i" -lt "${#q[@]}" ]; do
        pid=${q[i]}; i=$((i+1))
        line=""; read -r line 2>/dev/null < "/proc/$pid/stat"
        [ -n "$line" ] || continue
        rest=${line#*") "}                  # comm can hold spaces and parentheses
        read -ra a <<< "$rest"
        # utime stime cutime cstime, fields 14 to 17, counted from the end of comm
        [ "${#a[@]}" -ge 15 ] && total=$(( total + a[11] + a[12] + a[13] + a[14] ))
        for f in /proc/$pid/task/*/children; do
            [ -r "$f" ] || continue
            # The list has no trailing newline, so read reports failure having read it all.
            kids=""; read -r kids 2>/dev/null < "$f"
            for k in $kids; do q+=("$k"); done
        done
    done
    CPU_TICKS=$total
}

# Kernels built without CONFIG_PROC_CHILDREN cannot be asked downward, so the tree comes from
# every process's parent instead.
cpu_ticks_scan() {
    local pids
    pids=$(tree_pids "$1")
    { for p in $pids; do cat "/proc/$p/stat" 2>/dev/null; done; } | awk '
        {
            rest = substr($0, index($0, ") ") + 2)   # comm can hold spaces; skip past it
            n = split(rest, a, " ")
            if (n >= 15) sum += a[12] + a[13] + a[14] + a[15]
        }
        END {print sum + 0}'
}

counts_reaped_children() {
    read -r _ 2>/dev/null < /proc/$$/task/$$/children || [ -e /proc/$$/task/$$/children ]
}

ports_listening() {
    if command -v ss >/dev/null 2>&1; then
        ss -ltn 2>/dev/null | awk 'NR > 1 {n = split($4, a, ":"); print a[n]}'
    else
        # 0A is LISTEN. The port is hex, and converting it is bash's job: mawk has no strtonum.
        awk 'NR > 1 && $4 == "0A" {split($2, a, ":"); print a[2]}' /proc/net/tcp /proc/net/tcp6 2>/dev/null |
            while read -r x; do printf '%d\n' "$(( 16#$x ))"; done
    fi
}

load_x100() {   # the one-minute load average, times 100
    awk '{printf "%d", $1 * 100}' /proc/loadavg 2>/dev/null || echo 0
}

cpu_model() {
    awk -F': ' '/^model name/{print $2; exit}' /proc/cpuinfo 2>/dev/null
}

os_name() {
    (. /etc/os-release 2>/dev/null && echo "$ID $VERSION_ID") || echo unknown
}

abi_facts() {   # what beyond the kernel and arch decides whether a binary built elsewhere runs here
    # The x86-64 microarchitecture levels, the coarse ordering a binary is built against.
    # An approximation on purpose: -C target-cpu=native can reach past the level it lands
    # in, so equal levels make reuse plausible rather than proven.
    if [ -r /proc/cpuinfo ]; then
        awk '/^flags/ {
            for (i = 1; i <= NF; i++) f[$i] = 1
            lvl = 1
            if (f["cx16"] && f["lahf_lm"] && f["popcnt"] && f["sse4_1"] && f["sse4_2"] && f["ssse3"]) lvl = 2
            if (lvl == 2 && f["avx"] && f["avx2"] && f["bmi1"] && f["bmi2"] && f["f16c"] &&
                f["fma"] && f["abm"] && f["movbe"] && f["xsave"]) lvl = 3
            if (lvl == 3 && f["avx512f"] && f["avx512bw"] && f["avx512cd"] && f["avx512dq"] &&
                f["avx512vl"]) lvl = 4
            printf "level %d\n", lvl
            exit
        }' /proc/cpuinfo
    fi
    # A binary needs the glibc it was built against or newer, so this is an ordering too,
    # and its direction is most of the answer.
    ldd --version 2>/dev/null | awk 'NR == 1 { print "glibc " $NF; exit }'
}

shared_lock_howto() {
    note "Either give everyone this one account, which also lets one build cache"
    note "serve all of them, or make the lock directory shared. As root, idle:"
    note "  groupadd -f dibs && gpasswd -a <each-user> dibs"
    note "  install -d -m 2775 -g dibs $SHARED_DIR"
    note "  printf 'd $SHARED_DIR 2775 root dibs -\\n' > /etc/tmpfiles.d/dibs.conf"
    note "The last line recreates it on boot, since that tmpfs is emptied then."
}

pci_chip() {   # pci address; vendor:device of what the slot holds, empty for nothing; fails where slots cannot be read
    [ -d /sys/bus/pci/devices ] || return 1
    cat "/sys/bus/pci/devices/$1/vendor" "/sys/bus/pci/devices/$1/device" 2>/dev/null |
        sed 's/^0x//' | paste -sd: | tr 'A-F' 'a-f'
    return 0
}

# What makes two runs of one recipe incomparable: a governor other than performance, or another
# kernel or driver. Read from files, since it runs inside the exclusive lock.
machine_state() {   # key=value pairs for a measurement's record
    printf 'governor=%s kernel=%s nvidia=%s' \
        "$(cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | sort -u | paste -sd+ -)" \
        "$(uname -r)" \
        "$(grep -m1 -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' /proc/driver/nvidia/version 2>/dev/null | head -n 1)"
}

has_battery() {
    local b
    for b in /sys/class/power_supply/BAT*; do [ -e "$b" ] && return 0; done
    return 1
}

# What a card actually gets to the host, which is not what its own endpoint
# reports. A GPU with an internal bridge chain, as every Navi does, reports the
# x16 between its die and its own upstream port; the x1 riser above that is a
# hop further up and is the one that decides every transfer. So: the narrowest
# link on the path to the root, against what the card itself can do.
#
# Judged on width rather than speed, and the speed recorded is the link's
# ceiling rather than what it is doing now. An NVIDIA card idles its link a
# generation or two down and trains up under load, so "current" is a reading
# of how busy the machine was when probed, and recording it would make two
# probes of one unchanged machine disagree.
pcie_path() {  # bus; prints "width_to_host width_of_card speed_at_narrowest"
    local d=$1 w sp spn mw="" msp="" msn="" cap
    cap=$(cat "/sys/bus/pci/devices/$d/max_link_width" 2>/dev/null)
    while [ -n "$d" ] && [ -e "/sys/bus/pci/devices/$d" ]; do
        w=$(cat "/sys/bus/pci/devices/$d/current_link_width" 2>/dev/null)
        sp=$(cat "/sys/bus/pci/devices/$d/max_link_speed" 2>/dev/null)
        [ -n "$w" ] && { [ -z "$mw" ] || [ "$w" -lt "$mw" ] 2>/dev/null; } && mw=$w
        # Tracked separately from the width, and over the whole path: the hop that
        # narrows the link is not always the hop that slows it. A Navi card's own
        # bridge is gen4 while the root port it hangs off is gen3, so taking the
        # speed from wherever the width happened to drop reports the card's
        # internal ceiling as if it were the one to the host.
        spn=${sp%%.*}
        if [ -n "$spn" ] && { [ -z "$msn" ] || [ "$spn" -lt "$msn" ] 2>/dev/null; }; then
            # Space squeezed out: the caller word-splits this, and "8.0 GT/s"
            # would arrive as two fields and be dropped as a malformed answer.
            msn=$spn; msp=${sp% PCIe}; msp=${msp// /}
        fi
        d=$(basename "$(readlink -f "/sys/bus/pci/devices/$d/.." 2>/dev/null)")
        case $d in 0000:*) ;; *) break ;; esac
    done
    # An integrated GPU sits on the root complex with no link of its own and
    # says so as width 0 and a max of 255, the "not implemented" value. Nothing
    # here applies to it, and answering anyway divides by zero in the caller.
    [ "${mw:-0}" -ge 1 ] 2>/dev/null || return 0
    [ "${cap:-0}" -ge 1 ] 2>/dev/null && [ "$cap" -le 32 ] 2>/dev/null || return 0
    printf '%s %s %s\n' "$mw" "$cap" "${msp:-unknown}"
}

gpu_report() {   # what --check lists under devices; sets found, and what gpu_entries reads
    # Judged on output, never on the tool being installed or on its exit status. This
    # laptop has rocm-smi and no AMD GPU: it prints "Driver not initialized" and exits 0,
    # so asking either question gets a confident yes about hardware that is not there.
    found=0
    nv=$(nvidia-smi --query-gpu=name,pci.bus_id,memory.total,compute_cap \
         --format=csv,noheader,nounits 2>/dev/null)
    if [ -n "$nv" ]; then
        found=1
        printf '%s\n' "$nv" | while IFS=, read -r name busid mem cc; do
            printf '    gpu   %s  %s  %s MiB  sm%s\n' "$(echo $name)" "$(echo $busid)" "$(echo $mem)" "$(echo $cc)"
        done
    fi
    amd=$(rocm-smi --showproductname --csv 2>/dev/null | awk -F, 'NR>1 && NF>1 {print $2}')
    # rocm-smi is packaged on its own and reads the kernel driver, so it lists cards on a
    # machine with no HIP at all. What decides whether ROCm is a runtime here is the
    # runtime library, not the management tool.
    hip=0
    { command -v rocminfo >/dev/null 2>&1 ||
      ldconfig -p 2>/dev/null | grep -q libamdhip64; } && hip=1
    if [ -n "$amd" ]; then
        found=1
        if [ "$hip" = 1 ]; then
            printf '%s\n' "$amd" | sed 's/^/    gpu   /;s/$/  (rocm)/'
        else
            printf '%s\n' "$amd" | sed 's/^/    gpu   /;s/$/  (no rocm runtime)/'
            warn "rocm-smi sees these cards but nothing here can run HIP on them:"
            note "no rocminfo and no libamdhip64. They are Vulkan-only until the ROCm"
            note "runtime is installed, so a rocm label would have nowhere to go."
        fi
    fi
    # Reported for every card whatever vendor tool found it, because a narrowed link
    # is invisible to all of them: the card enumerates, works, and is simply slow.
    for lw in /sys/bus/pci/devices/*/current_link_width; do
        [ -e "$lw" ] || continue
        d=${lw%/current_link_width}
        case $(cat "$d/class" 2>/dev/null) in 0x030*) ;; *) continue ;; esac
        set -- $(pcie_path "${d##*/}")
        [ $# -eq 3 ] || continue
        [ "$1" -lt "$2" ] 2>/dev/null || continue
        warn "${d##*/} reaches the host over x$1, and the card can do x$2"
        note "Host transfers cost $(( $2 / $1 ))x what the card allows, so a benchmark"
        note "that moves data is measuring the riser or the slot it is in."
    done

    if [ "$found" = 0 ]; then
        if command -v lspci >/dev/null 2>&1; then
            lspci -nn 2>/dev/null | grep -Ei 'vga|3d controller' | sed 's/^/    gpu?  /' | head -8
            note "seen by lspci only: no vendor tool here can talk to them, so nothing"
            note "can be probed and no GPU work can be routed to this machine yet."
        else
            warn "no nvidia-smi, no rocm-smi, no lspci: cannot tell what is in this machine"
        fi
    fi
}

gpu_entries() {   # the inventory's device tables, from what gpu_report found
    have_vk=0; command -v vulkaninfo >/dev/null 2>&1 && have_vk=1
    # A chip id that appears twice is two cards of one model, and the slug it makes
    # then names neither of them.
    dup=$(lspci -nn 2>/dev/null |
          grep -Ei 'vga compatible controller|3d controller|display controller' |
          grep -oE '\[[0-9a-f]{4}:[0-9a-f]{4}\]' | sort | uniq -d | tr -d '[]')
    lspci -nn 2>/dev/null | grep -Ei 'vga compatible controller|3d controller|display controller' |
    while read -r line; do
        bus=${line%% *}
        case "$bus" in *:*:*) ;; *) bus="0000:$bus" ;; esac
        ids=$(printf '%s\n' "$line" | grep -oE '\[[0-9a-f]{4}:[0-9a-f]{4}\]' | tail -1 | tr -d '[]')
        [ -n "$ids" ] || continue
        desc=$(printf '%s\n' "$line" | sed 's/^[^]]*]: //; s/ \[[0-9a-f]\{4\}:[0-9a-f]\{4\}\].*$//')
        short=$(printf '%s\n' "$desc" | sed -n 's/.*\[\(.*\)\].*/\1/p')
        [ -n "$short" ] || short=$desc
        # nvidia-smi names the card better than lspci does, when it can see it,
        # and whether it can see it is also what decides CUDA below.
        seen=""
        if [ -n "$nv" ]; then
            seen=$(printf '%s\n' "$nv" | awk -F, -v b="$bus" \
                'tolower($2) ~ tolower(b) {gsub(/^ +| +$/,"",$1); print $1; exit}')
            [ -n "$seen" ] && short=$seen
        fi
        # What can reach this card, not what the machine has installed somewhere: an
        # AMD card in a machine with CUDA does not gain CUDA from it, and no runtime
        # reaches a card the kernel has no driver bound to.
        drv=""
        [ -L "/sys/bus/pci/devices/$bus/driver" ] &&
            drv=$(basename "$(readlink "/sys/bus/pci/devices/$bus/driver")")
        rt=""
        case "${ids%%:*}" in
            10de) [ -n "$seen" ] && rt='"cuda"' ;;
            1002) [ "$hip" = 1 ] && [ -n "$amd" ] && rt='"rocm"' ;;
        esac
        [ "$have_vk" = 1 ] && [ -n "$drv" ] && rt="${rt:+$rt, }\"vulkan\""
        slug=$(printf '%s\n' "$short" | tr 'A-Z' 'a-z' |
               sed 's/nvidia//g; s/geforce//g; s/corporation//g; s/advanced micro devices//g;
                    s/radeon//g; s/intel//g; s/ arc / /g' | tr -cd 'a-z0-9')
        printf '\n  [[machine.@NAME@.device]]\n'
        printf '  kind     = "gpu"\n'
        slug=${slug:-$(printf '%s' "$bus" | tr -cd 'a-z0-9')}
        case " $dup " in *" $ids "*) slug="$slug.$(printf '%s' "${bus#*:}" | cut -d: -f1)" ;; esac
        printf '  alias    = "gpu:%s"\n' "$slug"
        printf '  name     = "%s"\n' "$short"
        printf '  pci      = "%s"\n' "$bus"
        printf '  chip     = "%s"\n' "$ids"
        # What the card is actually plugged into. A mining rig puts cards on x1
        # risers, and a card on one is a card whose every host transfer runs at a
        # sixteenth of what its neighbour gets: a benchmark there measures the
        # riser. Width, not speed, is what is judged on below, because a link
        # downshifts its speed when idle and a single reading catches that.
        set -- $(pcie_path "$bus")
        [ $# -eq 3 ] && printf '  link     = "x%s of x%s at %s"\n' "$1" "$2" "$3"
        printf '  runtimes = [%s]\n' "$rt"
    done
}

fi
