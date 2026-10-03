# What differs between operating systems is behind the functions in PLATFORM_API, which each
# lib/machine/01-platform-<name>.sh defines for its own. Everything else assumes bash 5 and a GNU
# userland, which that file arranges where the system's own tools are something else.

# macOS's own bash is 3.2. This much parses in it, and bash reads a script one command at a time,
# so the rest of the file is never read by it.
if [ "${BASH_VERSINFO[0]}" -lt 5 ] && [ -z "${DIBS_REEXEC:-}" ]; then
    case "$0" in
        /*) for b in /opt/homebrew/bin/bash /usr/local/bin/bash; do
                [ -x "$b" ] && DIBS_REEXEC=1 exec "$b" "$0"
            done ;;
    esac
fi

case "$(uname -s)" in
    Linux) PLATFORM=linux ;;
    Darwin) PLATFORM=darwin ;;
    *) echo "dibs: $(uname -s) is not a platform dibs runs on yet: it needs a lib/machine/01-platform-<name>.sh" >&2
       exit 2 ;;
esac

PLATFORM_API="alive proc_state started_at pgid_of children_of fd_path tree_cpu_ticks
    counts_reaped_children lock_openers holds_flock ports_listening load_x100
    cpu_model os_name abi_facts has_battery pci_chip shared_lock_howto gpu_report gpu_entries
    machine_state stay_awake"
# macOS, read from ps, lsof, sysctl and pmset, with Homebrew's GNU userland first on the PATH.
if [ "$PLATFORM" = darwin ]; then

for d in /opt/homebrew/bin \
    /opt/homebrew/opt/gawk/libexec/gnubin /opt/homebrew/opt/grep/libexec/gnubin \
    /opt/homebrew/opt/gnu-sed/libexec/gnubin /opt/homebrew/opt/findutils/libexec/gnubin \
    /opt/homebrew/opt/coreutils/libexec/gnubin; do
    [ -d "$d" ] && PATH="$d:$PATH"
done
export PATH

alive() {   # pid
    ps -p "$1" >/dev/null 2>&1
}

proc_state() {   # pid; the one-letter state, empty once it has gone
    ps -o stat= -p "$1" 2>/dev/null | cut -c1
}

started_at() {   # pid; when it started, in seconds since the epoch
    local s
    s=$(LC_ALL=C ps -o lstart= -p "$1" 2>/dev/null)
    [ -n "$s" ] || return 1
    date -d "$s" +%s
}

pgid_of() {   # pid; prints its process group, empty if it has gone
    local g
    g=$(ps -o pgid= -p "$1" 2>/dev/null | tr -d ' ')
    [ -n "$g" ] && printf '%s\n' "$g"
}

# Which process holds an flock is only visible in Linux's /proc/<pid>/fdinfo, so here an orphan
# holding the lock is never found, and waits for a kill by hand.
holds_flock() {   # pid inode
    return 1
}

lock_openers() {   # file; the pids with it open
    lsof -t "$1" 2>/dev/null
}

children_of() {   # pid; its children, one per line
    pgrep -P "$1" 2>/dev/null
}

fd_path() {   # pid fd; what the descriptor points at, a path or something that is not one
    lsof -a -p "$1" -d "$2" -Fn 2>/dev/null | sed -n 's/^n//p' | head -1
}

# ps reports each living process's own CPU and nothing of the children it has reaped, so a job
# whose work runs in short-lived children reads as less busy than it is.
tree_cpu_ticks() {   # root pid; sets CPU_TICKS for it and everything under it
    local pids
    pids=$(tree_pids "$1")
    CPU_TICKS=$(ps -o time= -p "$(printf '%s,' $pids)" 2>/dev/null | awk -v clk="$CLK" '
        { n = split($1, t, ":"); s = t[n] + 60 * t[n-1] + (n > 2 ? 3600 * t[n-2] : 0); sum += s }
        END { printf "%d\n", sum * clk }')
}

counts_reaped_children() {
    return 1
}

ports_listening() {
    netstat -anp tcp 2>/dev/null | awk '$6 == "LISTEN" {n = split($4, a, "."); print a[n]}'
}

load_x100() {   # the one-minute load average, times 100
    sysctl -n vm.loadavg 2>/dev/null | awk '{printf "%d", $2 * 100}'
}

cpu_model() {
    sysctl -n machdep.cpu.brand_string 2>/dev/null
}

os_name() {
    printf 'macos %s\n' "$(sw_vers -productVersion 2>/dev/null)"
}

abi_facts() {
    :
}

shared_lock_howto() {
    note "Give everyone this one account, which also lets one build cache serve all of them."
}

pci_chip() {   # pci address; no slots to read here
    return 1
}

# Without a fan, a Mac's number depends on its power source and on whether it is throttling, and
# these are what a user can read of either.
machine_state() {   # key=value pairs for a measurement's record
    local power thermal
    power=$(pmset -g ps 2>/dev/null | sed -n "1s/.*'\(.*\) Power'.*/\1/p" | tr 'A-Z' 'a-z')
    thermal=$(pmset -g therm 2>/dev/null | sed -n 's/.*[Ww]arning [Ll]evel[^0-9]*\([0-9][0-9]*\).*/\1/p' | head -n 1)
    printf 'kernel=%s macos=%s power=%s lowpower=%s thermal=%s' "$(uname -r)" "$(sw_vers -productVersion 2>/dev/null)" \
        "$power" "$(pmset -g 2>/dev/null | awk '/lowpowermode/ {print $2}')" "${thermal:-nominal}"
}

# A Mac sleeps when nobody has touched it for a while, however busy it is, and a job it
# sleeps through is lost with its ssh.
stay_awake() {   # pid; keeps the machine up until it exits
    caffeinate -i -w "$1" </dev/null >/dev/null 2>&1 8>&- 9>&- 5<&- &
}

has_battery() {
    pmset -g batt 2>/dev/null | grep -q InternalBattery
}

gpu_report() {   # what --check lists under devices; sets found, and what gpu_entries reads
    found=0
    apple=$(system_profiler SPDisplaysDataType 2>/dev/null | awk -F': ' '
        /Chipset Model/ {m = $2} /Total Number of Cores/ {c = $2} /Metal Support/ {s = $2}
        END {if (m != "") printf "%s|%s|%s", m, c, s}')
    [ -n "$apple" ] || { warn "system_profiler lists no GPU"; return; }
    found=1
    IFS='|' read -r apple_gpu apple_cores apple_metal <<< "$apple"
    printf '    gpu   %s, %s cores, %s\n' "$apple_gpu" "${apple_cores:-?}" "${apple_metal:-Metal}"
}

# One GPU, so no slot to pin it by: the alias names it for the record, and --device on it pins
# nothing.
gpu_entries() {
    [ -n "${apple_gpu:-}" ] || return 0
    printf '\n  [[machine.@NAME@.device]]\n'
    printf '  kind     = "gpu"\n'
    printf '  alias    = "gpu:%s"\n' "$(printf '%s' "$apple_gpu" | tr 'A-Z' 'a-z' | sed 's/apple//' | tr -cd 'a-z0-9')"
    printf '  name     = "%s"\n' "$apple_gpu"
    printf '  runtimes = ["metal"]\n'
}

fi
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

stay_awake() {   # pid
    :
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
set -uo pipefail
for f in $PLATFORM_API; do
    declare -F "$f" >/dev/null || { echo "dibs: lib/machine/01-platform-$PLATFORM.sh defines no $f" >&2; exit 2; }
done
# The call arrives as assignments ahead of this script, one per variable it reads; set -u makes
# a missing one an error here rather than an empty value further down.
: "$MODE" "$LABEL" "$WAIT" "$MAXHOLD" "$VERBOSE" "$JSON" "$CMD" "$NO_WATCH" "$TTY" "$HOLD" \
  "$DEV_PCI" "$DEV_RT" "$DEV_NAME" "$DEV_CHIP" "$DEV_TWINS" "$STREAM" "$MAXFROM" "$FINGERPRINT"
AGENT=$(printf %s "$AGENT" | tr '\n\t' '  ' | cut -c1-48)
AGENT_ID=$(printf %s "$AGENT_ID" | tr '\n\t' '  ' | cut -c1-48)
BATCH_TAG=$(printf '%s\n' "$BATCH" | head -1 | tr '\t' ' ')
case "$LEASE" in ''|*[!0-9]*) LEASE=0 ;; esac
case "$READY_WITHIN" in ''|*[!0-9]*) READY_WITHIN=300 ;; esac
PORT_NUM=()
WITH_PID=(); WITH_LOG=(); WITH_END=(); WITH_BAD=(); WITH_UP=0
[ -n "$AGENT" ] || AGENT=?
# The caller cannot clean this up: its command line is read by fish, which has no $?.
# Unlinking a running script is safe; bash keeps the open inode.
trap 'rm -f "$0"' EXIT

# Where every user on this machine meets. A lock under /run/user or /tmp is keyed by uid, so
# two people each took their own, each was told the machine was idle, and both benchmarked at
# once: a wrong answer with nothing to notice it by. The shared directory has to be created
# once by root, which is why this prefers it and does not require it. Absent, the old per-user
# path is used unchanged, which is correct on a machine with one user and is what --check
# reports on a machine that may not stay that way.
#
# Still tmpfs, so it empties on reboot. That is not cosmetic: a record left in a persistent
# directory could name a pid that a later boot has reused, and prune, which asks /proc whether
# the pid is alive, would believe it.
# Overridable because /dev/shm is a Linux convention rather than a guarantee, and because a
# machine may want it elsewhere. It is a location, not a switch: pointing it at a directory
# that does not exist falls back exactly as if it were unset.
SHARED_DIR=${DIBS_SHARED_LOCK_DIR:-/dev/shm/dibs-lock}
if [ -n "${DIBS_LOCK_DIR:-}" ]; then
    DIR=$DIBS_LOCK_DIR; LOCK_SCOPE=explicit
elif [ -d "$SHARED_DIR" ] && [ -w "$SHARED_DIR" ]; then
    DIR=$SHARED_DIR; LOCK_SCOPE=shared
else
    DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/dibs-lock; LOCK_SCOPE=peruser
fi
mkdir -p "$DIR" 2>/dev/null || { DIR=/tmp/dibs-lock-$(id -u); LOCK_SCOPE=peruser; mkdir -p "$DIR"; }
# A lock nobody can write to is not a lock. mkdir -p succeeds on a directory that is already
# there whether or not it can be written, which is what a sandboxed shell sees: the real
# directory, read-only. Every write inside then fails, flock reports a bad file descriptor,
# the guards fall through one by one and the work runs unlocked beside whatever is being
# measured, reporting success. Falling back to another directory would be no better here,
# because the sessions that can write to this one would go on excluding each other and not us.
if ! : > "$DIR/.writable.$$" 2>/dev/null; then
    echo "dibs: $DIR cannot be written, so no lock can be taken. Nothing was run." >&2
    echo "  A sandboxed shell is the usual cause: it sees the lock directory and cannot" >&2
    echo "  write in it. Unlocked work beside a measurement is the one outcome this exists" >&2
    echo "  to prevent, and another directory would not exclude the sessions using this one." >&2
    exit 71
fi
rm -f "$DIR/.writable.$$" 2>/dev/null
# Records have to be removable by whoever prunes them, not only by whoever wrote them, or one
# user's dead job wedges the queue for everyone else.
[ "$LOCK_SCOPE" = shared ] && umask 002

# Timings outlive the machine, so they live off the tmpfs. Shared alongside the lock when there
# is a shared place to put them: an estimate built from everyone's runs of a label is a better
# estimate, and a log that only shows your own jobs cannot answer who is holding the machine.
SHARED_STATE=${DIBS_SHARED_STATE_DIR:-/var/lib/dibs}
if [ -n "${DIBS_HISTORY:-}" ] || [ -n "${DIBS_LOG:-}" ]; then
    HIST=${DIBS_HISTORY:-${XDG_STATE_HOME:-$HOME/.local/state}/dibs/history}
    LOG=${DIBS_LOG:-${XDG_STATE_HOME:-$HOME/.local/state}/dibs/log}
elif [ -d "$SHARED_STATE" ] && [ -w "$SHARED_STATE" ]; then
    HIST=$SHARED_STATE/history; LOG=$SHARED_STATE/log
else
    HIST=${XDG_STATE_HOME:-$HOME/.local/state}/dibs/history
    LOG=${XDG_STATE_HOME:-$HOME/.local/state}/dibs/log
fi
mkdir -p "$(dirname "$HIST")" "$(dirname "$LOG")" 2>/dev/null

# Every arrival and every outcome, so a job that was killed, wedged or orphaned still
# leaves a trace. The duration history next door only records what succeeded, which is
# precisely why the jobs worth investigating are the ones missing from it.
LOGGED_END=0
log_event() {   # event queued run exit
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$(date -Is)" "$1" "$$" "$MODE" "$LABEL" \
        "${2:--}" "${3:--}" "${4:--}" "${CMD_ONE:-${LABEL}}" \
        "${AGENT:-?}${DEV_NAME:+ on $DEV_NAME}" "${BATCH_TAG:--}" "${JOB:--}" >> "$LOG" 2>/dev/null
}

# Assigning rather than printing is the difference: show() renders several of these per
# line, and a command substitution forks the whole shell.
# Colour is for a person reading a terminal, so it appears only when the far end is one.
# Piped to a file, or read by the test suite, the output stays plain.
if [ "${TTY:-0}" = 1 ] && [ -z "${NO_COLOR:-}" ]; then
    C_OFF=$'\033[0m'; C_DIM=$'\033[2m'; C_B=$'\033[1m'
    C_BUSY=$'\033[1;31m'; C_FREE=$'\033[1;32m'; C_WARN=$'\033[33m'; C_Q=$'\033[36m'
    C_BENCH=$'\033[1;31m'; C_SHARED=$'\033[1;33m'
else
    C_OFF= C_DIM= C_B= C_BUSY= C_FREE= C_WARN= C_Q= C_BENCH= C_SHARED=
fi

# The two modes are the thing being scanned for, so they take the colours the first line
# already uses for them: a row reads the same way as the header that summarises it.
mode_hue() { case "$1" in bench) MC=$C_BENCH ;; rsh) MC=$C_Q ;; *) MC=$C_SHARED ;; esac; }

# One hue per agent, so the same session is the same colour everywhere it appears and two
# agents never read as one. Reds, greens and yellows are left out: they mean state here.
HUES=(33 39 63 99 105 135 170 176 205 38 44 111)
declare -A ACOLOUR=()
agent_hue() {   # name; sets AC
    AC=""
    [ -n "$C_OFF" ] || return
    local s=$1
    if [ -z "${ACOLOUR[$s]+set}" ]; then
        local i n h=7
        for (( i=0; i<${#s}; i++ )); do
            printf -v n '%d' "'${s:i:1}"
            h=$(( (h * 31 + n) & 0xffff ))
        done
        ACOLOUR[$s]=$'\033[38;5;'"${HUES[$(( h % ${#HUES[@]} ))]}"m
    fi
    AC=${ACOLOUR[$s]}
}

dur_() {   # var seconds
    local s=${2:-0}
    [ "$s" -lt 0 ] && s=0
    if   [ "$s" -ge 3600 ]; then printf -v "$1" '%dh%02dm' $((s/3600)) $((s%3600/60))
    elif [ "$s" -ge 60 ];   then printf -v "$1" '%dm%02ds' $((s/60)) $((s%60))
    else                         printf -v "$1" '%ds' "$s"
    fi
}
# Bash 5 keeps the wall clock in a variable, and show() reads it once per line.
now() { NOW=${EPOCHSECONDS:-$(date +%s)}; }
age_() { now; dur_ "$1" $(( NOW - $2 )); }
# The printing forms, for the once-per-job messages where a fork does not matter.
dur() { local d; dur_ d "$1"; printf '%s' "$d"; }
age() { local d; age_ d "$1"; printf '%s' "$d"; }

# Clock ticks per second, the unit tree_cpu_ticks counts in.
CLK=$(getconf CLK_TCK 2>/dev/null || echo 100)
# CPU seconds burned by a holder and everything under it; the platform decides how they are
# counted.
cpu_used() {   # sets CPU_TICKS and CPU_USED
    tree_cpu_ticks "$1"
    CPU_USED=$(( CPU_TICKS / CLK ))
}

# The tree under a pid, itself included, from every process's parent.
tree_pids() {   # root pid
    ps -eo pid=,ppid= | awk -v root="$1" '
        {pid[NR]=$1; par[NR]=$2}
        END {
            want[root]=1
            do {
                added=0
                for (i=1; i<=NR; i++)
                    if (want[par[i]] && !want[pid[i]]) { want[pid[i]]=1; added=1 }
            } while (added)
            for (i=1; i<=NR; i++) if (want[pid[i]]) print pid[i]
        }'
}

# Where a job's own output is going, walking its tree the way cpu_used does. Deeper first
# in the order they come back, because the redirect an agent wrote is always below the shells
# the wrapper puts in the way. A file is reported once however many descendants hold it open.
fd_targets() {   # root-pid channel; prints distinct regular files the tree writes to
    local -a q=("$1")
    local i=0 pid fd t k seen=" " out=""
    while [ "$i" -lt "${#q[@]}" ]; do
        pid=${q[i]}; i=$((i+1))
        for fd in 1 2; do
            t=$(fd_path "$pid" "$fd")
            case "$t" in
                /*) ;;                       # a pipe or socket reads as pipe:[n], not a path
                 *) continue ;;
            esac
            [ "$t" = "$2" ] && continue      # the channel it was launched down, not its own doing
            case "$t" in */jobs/*-*/log) continue ;; esac   # the job's own sink: dibs's doing, not the job's
            [ -f "$t" ] || continue          # /dev/null and the terminal are not output
            case "$seen" in *" $t "*) continue ;; esac
            seen="$seen$t "; out="$out$t"$'\n'
        done
        for k in $(children_of "$pid"); do q+=("$k"); done
    done
    printf '%s' "$out"
}

# The first file a job's tree is writing to, which is what --out would show. Separate from
# fd_targets' full list because the status wants one line, not an inventory.
holder_output() {   # pid; sets OUTPUT_FILE
    local root
    OUTPUT_FILE=
    root=$(fd_path "$1" 1)
    OUTPUT_FILE=$(fd_targets "$1" "$root" | grep -v '/with-[a-z0-9_]*\.log$' | head -1)
}

# A median moves only when a run finishes, and a run appends to the history before its
# holder file goes, so a redraw that finds the same entries in the lock directory is looking
# at the same history it computed against last time.
declare -A EST=()
EST_SIG=""
estimate() {   # mode label agent [fingerprint]; sets EST_V EST_N EST_SCOPE EST_OTHER, 1 with nothing to go on
    local key="$1/$2/${3-}/${4-}"
    [ -n "${EST[$key]+set}" ] || EST[$key]=$(estimate_compute "$1" "$2" "${3-}" "${4-}")
    [ -n "${EST[$key]}" ] || return 1
    read -r EST_LO EST_V EST_HI EST_N EST_SCOPE EST_OTHER <<< "${EST[$key]}"
}

# A median alone claims a confidence the history does not support. Half of these labels name
# a repo rather than a kind of work, so the same name covers a `git status` and a full build:
# `shared cubek` has 29 runs, nineteen of them instant and seven past two minutes. Its median
# is honestly zero and predicts nothing. Carrying the ninetieth percentile too lets the
# display say which numbers to trust, and gives a job past its median something better to be
# measured against than silence.
# Both tails, not just the high one. A sweep that has run in 20s and in 12m has a median
# right in the middle of a gap it has never actually landed in, and checking only the top
# would call that number trustworthy. The +1 is because durations are whole seconds, so a
# tenth percentile of zero is common and would otherwise make every ratio infinite.
est_wide() { [ "$EST_N" -ge 4 ] && [ "$EST_HI" -gt $(( (EST_LO + 1) * 3 )) ]; }

# Median rather than mean: one benchmark that hit a rebuild should not drag every estimate.
# Falls back from this exact label to the mode as a whole, and says nothing when it knows
# nothing, since a made-up number is worse than no number.
# Label, then agent, then mode. The label is the sharpest key and the one most often missing:
# two thirds of the labels ever recorded here appear exactly once, because a label describing
# one particular run files its duration where nothing will ever look it up again. What that
# agent's jobs usually take is a poorer answer than what this job usually takes, and a far
# better one than what every job on the machine usually takes.
estimate_compute() {   # mode label agent fingerprint
    [ -s "$HIST" ] || return 1
    local vals="" scope=this other=0
    # The sharpest key first, where the caller knows it: the same procedure with the same values,
    # which is what this job is about to do. One recipe measured on two backends is one label and
    # two costs, and a median across both predicts neither; a step added to a suite is the same
    # again. Both change the fingerprint, and neither should change the label.
    #
    # It falls back to the label the moment the narrow key is empty, so a procedure that has never
    # run is estimated exactly as it was before this existed, and nothing splinters.
    [ -n "${4-}" ] &&
        vals=$(awk -F'\t' -v m="$1" -v l="$2" -v f="$4" '$1==m && $2==l && $5==f {print $3}' "$HIST")
    # Marked, since the label's other values may do a quarter of this one's work or four times it.
    if [ -z "$vals" ]; then
        vals=$(awk -F'\t' -v m="$1" -v l="$2" '$1==m && $2==l {print $3}' "$HIST")
        [ -n "${4-}" ] && [ -n "$vals" ] && other=1
    fi
    if [ -z "$vals" ] && [ -n "${3-}" ]; then
        vals=$(awk -F'\t' -v m="$1" -v a="$3" '$1==m && $4==a {print $3}' "$HIST")
        scope=agent
    fi
    if [ -z "$vals" ]; then
        vals=$(awk -F'\t' -v m="$1" '$1==m {print $3}' "$HIST")
        scope=mode
    fi
    [ -z "$vals" ] && return 1
    printf '%s\n' "$vals" | sort -n | awk -v s="$scope" -v o="$other" '
        {a[NR]=$1}
        END {
            if (NR%2) m=a[(NR+1)/2]; else m=int((a[NR/2]+a[NR/2+1])/2)
            h=int(0.9*NR); if (h < 0.9*NR) h++   # nearest rank, so it never falls below the median
            if (h < 1) h=1
            l=int(0.1*NR); if (l < 1) l=1
            print a[l], m, a[h], NR, s, o
        }'
}

# A holder file outlives its process only if that process was killed outright, since the
# lock itself is the open descriptor. Trust the pid, not the file.
# A job of a cancelled batch that is holding the lock loses what runs under it and then exits
# 76 itself, so the lock is released the ordinary way. One still queued is killed outright,
# since its flock would otherwise return and the command would run unlocked. The mark refuses
# the batch's later steps here for a day, which is what stops a driver on another computer.
kill_batch_here() {   # id anyone
    local id=$1 anyone=$2 f pid mode start label agent who stopped=0 sig victim
    local -a jobs=()
    for f in "$DIR"/batch.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r bid _ < "$f"
        [ "$bid" = "$id" ] && jobs+=("${f##*.}")
    done
    for pid in "${jobs[@]}"; do
        f=$DIR/holder.$pid; [ -e "$f" ] || f=$DIR/waiting.$pid
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode _ start label agent who _ < "$f"
        if [ -z "$anyone" ] && { case "$who" in shell-*) true ;; *) [ -n "$who" ] && [ -n "$AGENT_ID" ] && [ "$who" != "$AGENT_ID" ] ;; esac; }; then
            echo "dibs: batch $id is running $label for $agent, not for you. If it should stop:" >&2
            echo "  dibs --kill $id --anyone" >&2
            exit 2
        fi
    done
    : > "$DIR/cancelled.$id"
    [ "$MODE" = kill-force ] && sig=KILL || sig=TERM
    for pid in "${jobs[@]}"; do
        if [ -e "$DIR/holder.$pid" ]; then
            for victim in $(tree_below "$pid" | tac); do kill -"$sig" "$victim" 2>/dev/null; done
        elif [ -e "$DIR/waiting.$pid" ]; then
            for victim in $(printf '%s\n' "$pid" $(tree_below "$pid") | tac); do kill -"$sig" "$victim" 2>/dev/null; done
        else
            continue
        fi
        stopped=$((stopped+1))
    done
    CMD_ONE="cancelled batch $id: $stopped job(s) stopped"
    LABEL=$id
    log_event cancelled
    echo "Cancelled batch $id on $(hostname -s): stopped $stopped job(s), and its later steps are refused here."
    exit 0
}

# A pid names one process only while it has the same start time. pid_max comes round in days on a
# busy machine, so a record left behind by a job that was killed outright, before its own cleanup
# could run, would otherwise name whoever holds that pid by then.
# A record is written after the process it names started, so one whose process is younger than the
# record itself is the remains of a job that has ended. Two seconds of slack for the rounding.
still_the_same() {   # pid record-file
    local began wrote
    began=$(started_at "$1") || return 1
    wrote=$(stat -c %Y "$2" 2>/dev/null) || return 0
    [ "$began" -le $(( wrote + 2 )) ]
}

prune() {
    local f pid
    find "$DIR" -maxdepth 1 -name 'cancelled.*' -mmin +1440 -delete 2>/dev/null
    for f in "$DIR"/port.*; do
        [ -e "$f" ] || continue
        pid=$(cat "$f" 2>/dev/null)
        { [ -n "$pid" ] && alive "$pid"; } || rm -f "$f"
    done
    for f in "$DIR"/holder.* "$DIR"/waiting.*; do
        [ -e "$f" ] || continue
        pid=${f##*.}
        still_the_same "$pid" "$f" || rm -f "$f"
    done
    for f in "$DIR"/cpu.* "$DIR"/batch.* "$DIR"/hold.* "$DIR"/with.*; do
        [ -e "$f" ] || continue
        pid=${f##*.}
        alive "$pid" || rm -f "$f"
    done
}

# A cumulative count can only answer whether a job has ever done anything, which catches one
# that wedged before it started and nothing else: the sweep that runs for an hour and then
# hangs has plenty of CPU behind it forever. Each look leaves behind what the tree had burned,
# so the next one can tell whether it has moved, and --watch leaves one every few seconds.
# Two observations are needed before there is anything to say, and it says nothing until then.
idle_check() {   # pid start; sets IDLE_FOR, IDLE_KIND and CPU_RATE
    local f=$DIR/cpu.$1 prev worked pts line=
    IDLE_FOR= IDLE_KIND= CPU_RATE=-1
    # 2>/dev/null ahead of the redirect, or the shell reports the first look at a holder,
    # when there is no sample yet, as an error.
    read -r line 2>/dev/null < "$f"
    read -r prev worked pts <<< "$line"
    case "${prev:-x}" in (*[!0-9]*) prev= ;; esac
    case "${worked:-x}" in (-) ;; (*[!0-9]*) worked= ;; esac
    case "${pts:-x}" in (*[!0-9]*) pts= ;; esac
    # Cores' worth, in hundredths, across the gap since the last look. The total on its own
    # says nothing about how hard a job is working: an hour of core-time means one thing over
    # ten minutes and quite another over ten hours.
    if [ -n "$prev" ] && [ -n "$pts" ] && [ "$NOW" -gt "$pts" ]; then
        CPU_RATE=$(( (CPU_TICKS - prev) * 100 / (CLK * (NOW - pts)) ))
    fi
    if [ "$CPU_TICKS" -eq 0 ]; then
        worked=-
    elif [ -z "$prev" ] || [ "$CPU_TICKS" -gt "$prev" ] || [ "$worked" = - ] || [ -z "$worked" ]; then
        worked=$NOW
    fi
    # Not rewritten within the same second, so two looks in quick succession still leave a
    # baseline far enough back to measure a rate against.
    [ "$NOW" -gt "${pts:-0}" ] && printf '%s %s %s\n' "$CPU_TICKS" "$worked" "$NOW" 2>/dev/null > "$f"
    if [ "$worked" = - ]; then
        IDLE_FOR=$(( NOW - $2 )); IDLE_KIND=never
    elif [ -n "$prev" ] && [ "$CPU_TICKS" -le "$prev" ]; then
        IDLE_FOR=$(( NOW - worked )); IDLE_KIND=stalled
    fi
    # A job still writing is a job still working, whatever its process tree says: a compiler
    # wrapper with a daemon, such as sccache, compiles outside the tree entirely, and reporting
    # that as stalled would have this tell you to kill a healthy build.
    if [ -n "$IDLE_FOR" ] && [ -n "${OUTPUT_FILE:-}" ] && [ -f "$OUTPUT_FILE" ]; then
        local wrote
        wrote=$(stat -c %Y "$OUTPUT_FILE" 2>/dev/null) || wrote=
        # Its own window, not the CPU one: they answer different questions, and sharing a
        # knob would mean tightening one silently disables the other.
        if [ -n "$wrote" ] && [ "$(( NOW - wrote ))" -lt "${DIBS_WROTE_WITHIN:-120}" ]; then
            IDLE_FOR= IDLE_KIND=
        fi
    fi
}

# Arrival order. It is what the queue looks like, not a promise about wake order. Insertion
# sort, since the queue is a handful of entries and sort(1) is a fork.
QF=()
queue_sorted() {
    local f j start rest
    local -a qs=()
    QF=()
    for f in "$DIR"/waiting.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r _ _ start rest < "$f"
        j=${#qs[@]}
        while [ "$j" -gt 0 ] && [ "${qs[j-1]}" -gt "$start" ]; do
            qs[j]=${qs[j-1]}; QF[j]=${QF[j-1]}; j=$((j-1))
        done
        qs[j]=$start; QF[j]=$f
    done
}

# Openers are not holders. A lock descriptor is inherited, and this can run inside a client
# that holds one, so the whole invocation asking the question shows up as openers: the client,
# the script it ships, and the children of the pipeline that calls fuser. Which of them holds
# the lock is in the descriptor itself, since the kernel reports a file's locks under fdinfo:
# /proc/locks cannot answer it, naming the flock helper that took the lock and then exited,
# never the process whose descriptor keeps it. A process group is a second guard, because an
# orphan is by definition the remains of a session that has gone. Beyond that, a queued client
# has a record, and a pid that has already exited holds nothing, which is the rule prune
# follows for the same reason.
tree_below() {   # pid; its descendants, parents before children, the pid itself excluded
    ps -eo pid=,ppid= | awk -v root="$1" '
        {child[NR]=$1; parent[NR]=$2}
        END {
            want[root]=1
            do {
                added=0
                for (i=1; i<=NR; i++)
                    if (want[parent[i]] && !want[child[i]]) {
                        want[child[i]]=1; printf "%s ", child[i]; added=1
                    }
            } while (added)
        }'
}

# The lock is released the moment a job exits, and a grandchild still running then runs unlocked
# beside the next measurement, so everything below goes first, then what was named: TERM, and
# KILL ten seconds later for whatever ignores it.
reap() {   # pids
    local round below p sig alive st
    for round in $(seq 50); do
        below=$(for p in "$@"; do tree_below "$p"; done)
        [ -n "$below" ] || break
        [ "$round" -gt 40 ] && sig=KILL || sig=TERM
        for p in $(printf '%s\n' $below | tac); do kill -"$sig" "$p" 2>/dev/null; done
        sleep 0.25
    done
    kill -TERM "$@" 2>/dev/null
    for round in $(seq 41); do
        # A zombie has ended, and outside its parent kill -0 would still find it.
        alive=$(for p in "$@"; do
                    st=$(proc_state "$p")
                    [ -n "$st" ] && [ "$st" != Z ] && echo "$p"
                done)
        [ -n "$alive" ] || return 0
        [ "$round" = 41 ] && kill -KILL $alive 2>/dev/null
        sleep 0.25
    done
}

# A port two jobs both chose is the collision this exists to prevent, so the reservation is an
# exclusive create under the lock directory, and it names the pid holding it so prune can clear it.
ports_take() {   # 1 when the range has nothing free
    local lo hi used i try p range=${DIBS_PORTS:-20500-20999}
    lo=${range%%-*}; hi=${range##*-}
    used=" $(ports_listening | tr '\n' ' ') "
    for i in "${!PORT_NAME[@]}"; do
        PORT_NUM[$i]=""
        for try in $(seq 200); do
            p=$(( lo + RANDOM % (hi - lo + 1) ))
            case "$used" in *" $p "*) continue ;; esac
            ( set -o noclobber; echo $$ > "$DIR/port.$p" ) 2>/dev/null || continue
            PORT_NUM[$i]=$p
            used="$used$p "
            break
        done
        [ -n "${PORT_NUM[$i]}" ] || return 1
        export "DIBS_PORT_$(printf %s "${PORT_NAME[$i]}" | tr 'a-z' 'A-Z')=${PORT_NUM[$i]}"
    done
}

port_of() {   # name; the port picked for it, or the name back when it is a number already
    local i
    for i in "${!PORT_NAME[@]}"; do
        [ "${PORT_NAME[$i]}" = "$1" ] && { printf '%s' "${PORT_NUM[$i]}"; return 0; }
    done
    printf '%s' "$1"
}

with_lines() {   # pid
    local n p c
    [ -e "$DIR/with.$1" ] || return 0
    while IFS=$'\t' read -r n p c; do
        echo "    ${C_DIM}with $n, pid $p:$C_OFF $C_DIM$c$C_OFF"
    done < "$DIR/with.$1"
}

with_start() {
    local i
    WITH_UP=1
    WITH_STARTED=$(date +%s)
    for i in "${!WITH_NAME[@]}"; do
        WITH_LOG[$i]=/dev/null
        [ -n "$JOBLOG" ] && WITH_LOG[$i]=$JOBDIR/with-${WITH_NAME[$i]}.log
        bash -c "${WITH_CMD[$i]}" 8>&- 9>&- 5<&- < /dev/null > "${WITH_LOG[$i]}" 2>&1 &
        WITH_PID[$i]=$!
        printf '%s\n' "$!" >> "$WORKFILE"
        printf '%s\t%s\t%s\n' "${WITH_NAME[$i]}" "$!" \
            "$(printf %s "${WITH_CMD[$i]}" | tr '\n\t' '  ' | cut -c1-160)" >> "$DIR/with.$$"
    done
}

with_answers() {   # readiness seconds; readiness is empty, tcp:[host:]port, or a command that exits 0
    local a h=127.0.0.1
    case "$1" in
        '') return 0 ;;
        tcp:*) a=${1#tcp:}
               case "$a" in *:*) h=${a%:*}; a=${a##*:} ;; esac
               case "$a" in *[!0-9]*) a=$(port_of "$a") ;; esac
               timeout "$2" bash -c 'exec 3<>"/dev/tcp/$1/$2"' _ "$h" "$a" 8>&- 9>&- 5<&- 2>/dev/null ;;
        *) timeout --kill-after=2 "$2" bash -c "$1" 8>&- 9>&- 5<&- < /dev/null > /dev/null 2>&1 ;;
    esac
}

with_failed() {   # index what consequence; with the end of its log, which is usually where the reason is
    WITH_END[$1]="${WITH_END[$1]:+${WITH_END[$1]}, }$2"
    WITH_BAD[$1]=1
    WITH_FAIL="service ${WITH_NAME[$1]} $2"
    echo "dibs: $WITH_FAIL, so $3." >&2
    if [ -s "${WITH_LOG[$1]}" ]; then
        echo "  The end of its log, ${WITH_LOG[$1]}:" >&2
        tail -n 10 "${WITH_LOG[$1]}" | sed 's/^/    /' >&2
    fi
}

with_ready() {   # 1 when a service is not there to be used
    local i st left deadline=$(( $(date +%s) + READY_WITHIN ))
    for i in "${!WITH_NAME[@]}"; do
        until left=$(( deadline - $(date +%s) )); with_answers "${WITH_READY[$i]}" $(( left > 0 ? left : 1 )); do
            if ! kill -0 "${WITH_PID[$i]}" 2>/dev/null; then
                wait "${WITH_PID[$i]}"; st=$?
                with_failed "$i" "exited $st before it was ready" "the command did not run"
                return 1
            fi
            if [ "$(date +%s)" -ge "$deadline" ]; then
                with_failed "$i" "was not ready within ${READY_WITHIN}s" "the command did not run"
                return 1
            fi
            sleep 0.2
        done
        WITH_END[$i]="ready after $(( $(date +%s) - WITH_STARTED ))s"
    done
}

with_stop() {
    local i alive=()
    for i in "${!WITH_PID[@]}"; do
        kill -0 "${WITH_PID[$i]}" 2>/dev/null || continue
        alive+=("${WITH_PID[$i]}")
        if [ -n "${WITH_BAD[$i]:-}" ]; then WITH_END[$i]="${WITH_END[$i]}, and stopped"
        else WITH_END[$i]="${WITH_END[$i]:+${WITH_END[$i]}, }stopped when the command ended"; fi
    done
    [ "${#alive[@]}" -gt 0 ] && { reap "${alive[@]}"; wait "${alive[@]}" 2>/dev/null; }
    rm -f "$DIR/with.$$"
    WITH_UP=0
}

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

# The sweep runs as the job, on the side that has the directory: what fills a machine is its
# own worktrees, caches and logs, and the clocks are its environment's to set. Two of them,
# since a build cache is refilled by a compiler and a worktree is not.
gc_script() {   # days dry
    cat <<GCTOP
KEEP=\${DIBS_KEEP_DAYS:-14}
TKEEP=\${DIBS_TARGET_KEEP_DAYS:-5}
DRY=$2
GCTOP
    [ "$1" = default ] || printf 'KEEP=%s\nTKEEP=%s\n' "$1" "$1"
    cat <<'GCEND'
    S=${DIBS_SCRATCH:-$HOME/.cache/dibs}
    [ -d "$S" ] || { echo "dibs: nothing at $S to sweep."; exit 0; }
    now=$(date +%s)
    total=0 freed=0 would=0
    declare -A MB
    # One du for a whole category rather than one per entry: the listing is the point of the
    # command, and a fork per build cache is the part that made it feel expensive.
    measure() {   # paths
        local m d
        [ $# -gt 0 ] || return 0
        while read -r m d; do MB[$d]=$m; total=$(( total + m )); done < <(du -sk "$@" 2>/dev/null)
    }
    size() {   # kibibytes as the sizes a person acts on
        local k=${1:-0}
        if [ "$k" -ge 1048576 ]; then printf '%d.%dG' $(( k / 1048576 )) $(( k % 1048576 * 10 / 1048576 ))
        elif [ "$k" -ge 1024 ]; then printf '%dM' $(( k / 1024 ))
        else printf '%dK' "$k"; fi
    }
    ago() {
        local d=$(( (now - ${1:-$now}) / 86400 ))
        case "$d" in 0) printf today ;; 1) printf yesterday ;; *) printf '%s days ago' "$d" ;; esac
    }
    # Biggest first, since the question behind the command is where the disk went, and a tail
    # nobody would act on is counted rather than printed. What is past its clock is always named,
    # however small: it is about to go, or would.
    ROWS=()
    row() {   # kib past line
        ROWS+=("$1"$'\t'"$2"$'\t'"$3")
    }
    rows_out() {   # heading, printed only if there is anything under it
        local n=0 rest=0 restk=0 k past line
        [ "${#ROWS[@]}" -gt 0 ] || return 0
        echo "$1"
        while IFS=$'\t' read -r k past line; do
            if [ "$n" -lt 20 ] || [ "$past" = 1 ]; then printf '%s\n' "$line"; n=$(( n + 1 ))
            else rest=$(( rest + 1 )); restk=$(( restk + k )); fi
        done < <(printf '%s\n' "${ROWS[@]}" | sort -rn -k1,1)
        [ "$rest" -gt 0 ] && printf '    and %s more holding %s, none of it past its clock\n' \
            "$rest" "$(size "$restk")"
        ROWS=()
    }
    used() {   # the marker dibs leaves when it prepares, else the directory itself
        local t
        t=$(stat -c %Y "$1/.dibs-used" 2>/dev/null) || t=$(stat -c %Y "$1" 2>/dev/null) || t=$now
        printf %s "$t"
    }
    echo "dibs --gc on $(hostname -s), under $S"

    # A worktree is git's to remove, and one git has lost is a plain directory. The clone it was
    # added from keeps a registration either way, which is what the prune below is for.
    tore_out=""
    measure "$S"/ws/*/*
    for d in "$S"/ws/*/*; do
        [ -d "$d" ] || continue
        # One with no marker predates the marker, so it is dated rather than deleted.
        [ -e "$d/.dibs-used" ] || touch "$d/.dibs-used"
        t=$(used "$d"); verdict=""
        if [ $(( (now - t) / 86400 )) -gt "$KEEP" ]; then
            if [ "$DRY" = 1 ]; then verdict="   would remove"; would=$(( would + ${MB[$d]:-0} ))
            else
                git -C "$d" worktree remove --force "$d" 2>/dev/null || rm -rf "$d"
                verdict="   removed"; freed=$(( freed + ${MB[$d]:-0} ))
                r=${d%/*}; tore_out="$tore_out ${r##*/}"
            fi
        fi
        row "${MB[$d]:-0}" "$([ -n "$verdict" ] && echo 1 || echo 0)" \
            "$(printf '    %-40s %7s  used %s%s' "${d#"$S"/}" "$(size "${MB[$d]:-0}")" "$(ago "$t")" "$verdict")"
    done
    rows_out "  worktrees, removed after ${KEEP} days unused"
    for r in $(printf '%s\n' $tore_out | sort -u); do
        git -C "$HOME/prog/$r" worktree prune 2>/dev/null
    done

    # A cache seeded from a sibling shares the sibling's blocks, which du counts in both. Where
    # the filesystem shares blocks, the extent map says which are a cache's alone: what removing
    # it frees. Shared extents are counted once, by where they sit on the disk.
    declare -A OWN
    shared=0
    share_scan() {   # dirs
        local d
        for d in "$@"; do
            [ -d "$d" ] || continue
            printf 'DIBS-DIR %s\n' "$d"
            find "$d" -type f -printf '%i %p\n' 2>/dev/null | sort -u -k1,1 | cut -d' ' -f2- |
                xargs -r -d '\n' filefrag -v -b1024 2>/dev/null
        done | awk -F: '
            /^DIBS-DIR / { if (d != "") print own + 0, d; d = substr($0, 10); own = 0; next }
            /^ *[0-9]+:/ { if ($NF ~ /shared/) { split($3, p, "."); sh[p[1] + 0] = $4 + 0 } else own += $4 }
            END { if (d != "") print own + 0, d; for (s in sh) t += sh[s]; print t + 0, "*" }'
    }
    tfs=$(stat -f -c %T "$S/target/." 2>/dev/null)
    before=$total
    measure "$S"/target/*
    if { [ "$tfs" = xfs ] || [ "$tfs" = btrfs ]; } && command -v filefrag >/dev/null; then
        while read -r k d; do
            if [ "$d" = "*" ]; then shared=$k; else OWN[$d]=$k; fi
        done < <(share_scan "$S"/target/*)
        together=$shared
        for d in "${!OWN[@]}"; do together=$(( together + OWN[$d] )); done
        total=$(( before + together ))
    fi
    for d in "$S"/target/*; do
        [ -d "$d" ] || continue
        [ -e "$d/.dibs-used" ] || echo swept > "$d/.dibs-used"
        t=$(used "$d"); verdict=""; k=${OWN[$d]:-${MB[$d]:-0}}
        if [ $(( (now - t) / 86400 )) -gt "$TKEEP" ]; then
            if [ "$DRY" = 1 ]; then verdict="   would remove"; would=$(( would + k ))
            else rm -rf "$d"; verdict="   removed"; freed=$(( freed + k )); fi
        fi
        own=""; [ -n "${OWN[$d]:-}" ] && own=$(printf '  own %7s' "$(size "${OWN[$d]}")")
        row "$k" "$([ -n "$verdict" ] && echo 1 || echo 0)" \
            "$(printf '    %-40s %7s%s  used %s%s' "${d#"$S"/}" "$(size "${MB[$d]:-0}")" "$own" "$(ago "$t")" "$verdict")"
    done
    rows_out "  build caches on $(df --output=target "$S/target/." 2>/dev/null | tail -1), removed after ${TKEEP} days unused"
    [ "${#OWN[@]}" -gt 0 ] &&
        printf '    together %s on the disk: own is what removing that cache alone frees, and %s is shared among them\n' \
            "$(size "$together")" "$(size "$shared")"

    # Counted rather than listed, all of them being alike and there being hundreds: the one job
    # anybody wants is found by its id with dibs out, never by reading this.
    bulk() {   # clock label paths
        local clock=$1 what=$2 d t a n=0 m=0 on=0 om=0 oldest=0
        shift 2
        measure "$@"
        for d in "$@"; do
            [ -e "$d" ] || continue
            n=$(( n + 1 )); m=$(( m + ${MB[$d]:-0} ))
            t=$(stat -c %Y "$d" 2>/dev/null) || continue
            a=$(( (now - t) / 86400 ))
            [ "$a" -gt "$oldest" ] && oldest=$a
            [ "$a" -gt "$clock" ] || continue
            on=$(( on + 1 )); om=$(( om + ${MB[$d]:-0} ))
            if [ "$DRY" = 1 ]; then would=$(( would + ${MB[$d]:-0} ))
            else rm -rf "$d" && freed=$(( freed + ${MB[$d]:-0} )); fi
        done
        [ "$n" -gt 0 ] || return 0
        printf '  %s, removed after %s days: %s %s, %s, oldest %s days' \
            "$what" "$clock" "$n" "$([ "$n" = 1 ] && echo entry || echo entries)" "$(size "$m")" "$oldest"
        if [ "$on" = 0 ]; then printf ', none past its clock\n'
        elif [ "$DRY" = 1 ]; then printf ', %s past it holding %s, which would go\n' "$on" "$(size "$om")"
        else printf ', removed %s holding %s\n' "$on" "$(size "$om")"; fi
    }
    bulk "$KEEP" "job logs and artifacts" "$S"/jobs/*
    bulk "$KEEP" "leftover temporary files" "$S"/tmp/* "$S"/out/*

    # Nothing dibs made, so nothing dibs deletes: a directory somebody wrote by hand may be the
    # only copy of what they are working on, and a machine is shared. Sized and dated, which is
    # what makes it possible to go and ask them.
    other=()
    for d in "$S"/*; do
        case "${d##*/}" in ws|target|jobs|tmp|out) continue ;; esac
        [ -e "$d" ] || continue
        other+=("$d")
    done
    if [ "${#other[@]}" -gt 0 ]; then
        measure "${other[@]}"
        for d in "${other[@]}"; do
            row "${MB[$d]:-0}" 0 "$(printf '    %-40s %7s  written %s' "${d#"$S"/}" \
                "$(size "${MB[$d]:-0}")" "$(ago "$(stat -c %Y "$d" 2>/dev/null || echo "$now")")")"
        done
        rows_out "  not dibs's, never removed by this"
    fi

    if [ "$DRY" = 1 ]; then
        printf '  %s of %s is past its clock and would go. Run it without --dry-run.\n' "$(size "$would")" "$(size "$total")"
    else
        printf '  reclaimed %s of %s\n' "$(size "$freed")" "$(size "$total")"
    fi
    # The build caches are often a disk of their own, linked in under the scratch directory.
    df -h "$S/." "$S/target/." 2>/dev/null | awk 'NR > 1 && !seen[$6]++ {printf "  %s free of %s on %s\n", $4, $2, $6}'
GCEND
}

# One document per line, so a reader can take it a line at a time. Built with printf and
# parameter expansion rather than a JSON tool, because this runs on every --watch tick and a
# fork per field would undo the whole point of the redraw being free.
jstr() {   # var value
    local v=${2-}
    v=${v//\\/\\\\}
    v=${v//\"/\\\"}
    printf -v "$1" '"%s"' "$v"
}
# Two renderers asking the same question in two places is exactly how they come to disagree:
# --json called a job overrunning while --status said nothing, for every label whose recorded
# runs were all under a second and whose median is therefore zero.
# Judged against the slowest this label has ever honestly been, not against its middle: a
# label whose runs range twenty seconds to twelve minutes has no business calling a two
# minute run stuck.
overrunning() {   # elapsed
    [ "$EST_SCOPE" = this ] && [ "$EST_OTHER" = 0 ] && [ "$EST_HI" -gt 0 ] && [ "$1" -gt $(( EST_HI * 2 )) ]
}

# Past the median the job is in the tail, and the tail still has a shape. Surrendering there
# cost every waiter its ETA, because the labels that hold the machine longest are exactly the
# ones whose median is zero. Zero remains a wrong answer, reading as "any moment now".
remaining_of() {   # elapsed; sets REM, -1 when nothing can be said, and REM_KIND
    REM_KIND=typical
    if [ "$1" -lt "$EST_V" ]; then REM=$(( EST_V - $1 ))
    elif [ "$1" -lt "$EST_HI" ]; then REM=$(( EST_HI - $1 )); REM_KIND=bound
    else REM=-1; REM_KIND=
    fi
}

# Queued shared jobs are not standing in line behind one another. The shared lock admits all
# of them the moment it is free, so a run of them starts together and the queue only actually
# advances at a benchmark, which has to wait for everything already admitted to drain.
# Adding their durations up told the third shared job it was waiting out the first two.
queue_eta_start() {   # free known
    QE_AT=$1 QE_KNOWN=$2 QE_PENDING=0 QE_PENDING_KNOWN=1
}
queue_eta() {   # mode label agent; sets QE_ETA, -1 when it cannot be said
    local have=1
    estimate "$1" "$2" "$3" || have=0
    if [ "$1" != bench ]; then
        [ "$QE_KNOWN" = 1 ] && QE_ETA=$QE_AT || QE_ETA=-1
        if [ "$have" = 1 ]; then
            [ "$EST_V" -gt "$QE_PENDING" ] && QE_PENDING=$EST_V
        else
            QE_PENDING_KNOWN=0   # only matters to a benchmark queued behind it
        fi
        return
    fi
    if [ "$QE_KNOWN" = 1 ] && [ "$QE_PENDING_KNOWN" = 1 ]; then
        QE_ETA=$(( QE_AT + QE_PENDING ))
        [ "$have" = 1 ] && QE_AT=$(( QE_ETA + EST_V )) || QE_KNOWN=0
    else
        QE_ETA=-1 QE_KNOWN=0
    fi
    QE_PENDING=0 QE_PENDING_KNOWN=1
}

# What a caller that has just been queued is told, in one line: what holds the machine, how many
# wait ahead of it and when it should start. The whole picture is dibs status, or -v here.
queued_line() {
    local f mode pid start label agent first= n=0 ahead=0 free=0 known=1 eta=-1 line d
    prune
    now
    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent _ < "$f"
        n=$((n+1))
        [ -n "$first" ] || { [ "$mode" = bench ] && first="the benchmark $label" || first=$label; }
        if estimate "$mode" "$label" "$agent"; then
            remaining_of $(( NOW - start ))
            if [ "$REM" -lt 0 ]; then known=0; elif [ "$REM" -gt "$free" ]; then free=$REM; fi
        else
            known=0
        fi
    done
    queue_sorted
    queue_eta_start "$free" "$known"
    for f in "${QF[@]}"; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent _ < "$f"
        queue_eta "$mode" "$label" "$agent"
        [ "$pid" = "$$" ] && { eta=$QE_ETA; break; }
        ahead=$((ahead+1))
    done
    # Nothing holds it on the record and yet it is taken: the wait ahead is not a queue but a
    # wedge, and it lasts until the orphan goes. Said here because this is the line the caller
    # reads before settling in for twenty minutes.
    if [ "$n" = 0 ]; then
        lock_unaccounted
        [ -n "$ORPH" ] && {
            printf 'dibs: the lock is held by an orphan (pid%s), which left no record, so nothing here can start. Reclaim it with: dibs --release\n' "$ORPH"
            return 0; }
    fi
    line="dibs: queued and has not started, behind ${first:-a job starting up}"
    [ "$n" -gt 1 ] && line="$line and $((n-1)) more"
    [ "$ahead" -gt 0 ] && line="$line, with $ahead queued first"
    if [ "$eta" -gt 0 ]; then dur_ d "$eta"; line="$line, ~$d until it starts"; fi
    printf '%s. dibs status shows the queue.\n' "$line"
}

# Where the queue will stand once everything waiting now has started, as queue_eta leaves it. A
# batch's steps still to come arrive after all of it, so they are placed behind it.
queue_tail() {
    local f mode start label agent free=0 known=1
    QT_EXCL=0
    now
    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode _ start label agent _ < "$f"
        [ "$mode" = bench ] && QT_EXCL=1
        if estimate "$mode" "$label" "$agent"; then
            remaining_of $(( NOW - start ))
            [ "$REM" -lt 0 ] && known=0
            [ "$REM" -gt "$free" ] && free=$REM
        else
            known=0
        fi
    done
    queue_sorted
    queue_eta_start "$free" "$known"
    for f in "${QF[@]}"; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode _ _ label agent _ < "$f"
        [ "$mode" = bench ] && QT_EXCL=1
        queue_eta "$mode" "$label" "$agent"
    done
    QT_AT=$QE_AT QT_KNOWN=$QE_KNOWN QT_PENDING=$QE_PENDING QT_PKNOWN=$QE_PENDING_KNOWN
}

batch_tail() {
    local f
    QT_AT=0 QT_KNOWN=0 QT_PENDING=0 QT_PKNOWN=0 QT_EXCL=0
    for f in "$DIR"/batch.*; do
        [ -e "$f" ] && { queue_tail; return; }
    done
}

# A batch reaches the machine one step at a time, so without its plan a job looks like the last
# thing its agent asked for, and the time that agent has left reads as this job's alone. Only a
# label's own history counts toward it: what an agent or the machine usually takes says nothing
# about a step that has never run.
batch_read() {   # pid left-of-this-job; sets BT_*, 1 when the job is not part of a batch
    local f=$DIR/batch.$1 name mode label here d start
    local at=${QT_AT:-0} known=${QT_KNOWN:-0} pend=${QT_PENDING:-0} pknown=${QT_PKNOWN:-0} excl=${QT_EXCL:-0}
    BT_ID= BT_STEP= BT_K= BT_N= BT_HERE=0 BT_NEXT= BT_FARN=0 BT_FAR= BT_LEFT=$2 BT_PARTIAL=0
    [ -s "$f" ] || return 1
    {
        IFS=$'\t' read -r BT_ID BT_STEP
        IFS=$'\t' read -r BT_K BT_N
        while IFS=$'\t' read -r name mode label here; do
            [ -n "$name" ] || continue
            if [ "$here" != 1 ]; then
                BT_FARN=$((BT_FARN+1))
                [ "$BT_FARN" -le 6 ] && BT_FAR="$BT_FAR${BT_FAR:+, }$name"
                continue
            fi
            BT_HERE=$((BT_HERE+1))
            if [ "$mode" = peek ]; then
                [ "$BT_HERE" -le 6 ] && BT_NEXT="$BT_NEXT${BT_NEXT:+, }$name"
                continue
            fi
            # A shared step waits only while a benchmark holds or is queued; a benchmark waits
            # for everything admitted before it. Where the queue cannot be estimated the step
            # is placed as if it were empty, and the total becomes a floor.
            start=$BT_LEFT
            if [ "$BT_LEFT" -ge 0 ]; then
                if [ "$mode" = bench ]; then
                    if [ "$known" = 1 ] && [ "$pknown" = 1 ]; then
                        [ $(( at + pend )) -gt "$start" ] && start=$(( at + pend ))
                    else
                        BT_PARTIAL=1
                    fi
                elif [ "$excl" = 1 ]; then
                    if [ "$known" = 1 ]; then
                        [ "$at" -gt "$start" ] && start=$at
                    else
                        BT_PARTIAL=1
                    fi
                fi
            fi
            if estimate "$mode" "$label" "" && [ "$EST_SCOPE" = this ]; then
                dur_ d "$EST_V"; d=" ~$d"
                if [ "$BT_LEFT" -ge 0 ]; then
                    BT_LEFT=$(( start + EST_V ))
                    if [ "$mode" = bench ]; then
                        at=$BT_LEFT pend=0 known=1 pknown=1
                    elif [ $(( BT_LEFT - at )) -gt "$pend" ]; then
                        pend=$(( BT_LEFT - at ))
                    fi
                fi
            else
                d=" (no history)"; BT_PARTIAL=1
                [ "$BT_LEFT" -ge 0 ] && BT_LEFT=$start
            fi
            [ "$BT_HERE" -le 6 ] && BT_NEXT="$BT_NEXT${BT_NEXT:+, }$name$d"
        done
    } < "$f"
    [ "$BT_HERE" -gt 6 ] && BT_NEXT="$BT_NEXT and $(( BT_HERE - 6 )) more"
    [ "$BT_FARN" -gt 6 ] && BT_FAR="$BT_FAR and $(( BT_FARN - 6 )) more"
    BT_K=${BT_K//[^0-9]/} BT_N=${BT_N//[^0-9]/}
    [ -n "$BT_ID" ]
}

batch_lines() {   # pid left-of-this-job
    batch_read "$1" "$2" || return 0
    local l
    echo "    ${C_DIM}batch$C_OFF $BT_ID${C_DIM}, step ${BT_K:-?} of ${BT_N:-?}: $BT_STEP$C_OFF"
    [ "$BT_HERE" -gt 0 ] && echo "    ${C_DIM}then here:$C_OFF $BT_NEXT"
    [ "$BT_FARN" -gt 0 ] && echo "    ${C_DIM}then on other machines:$C_OFF $BT_FAR"
    if [ "$BT_LEFT" -lt 0 ]; then
        echo "    ${C_DIM}batch time left here: unknown, this step's own time cannot be estimated$C_OFF"
    else
        dur_ l "$BT_LEFT"
        if [ "$BT_PARTIAL" = 1 ]; then
            echo "    ${C_DIM}batch time left here: over $l, since some of what is ahead has no history$C_OFF"
        else
            echo "    ${C_DIM}batch time left here: ~$l$C_OFF"
        fi
    fi
}

batch_json() {   # pid left-of-this-job
    batch_read "$1" "$2" || return 0
    local ji js jn jf
    jstr ji "$BT_ID"; jstr js "$BT_STEP"; jstr jn "$BT_NEXT"; jstr jf "$BT_FAR"
    printf ',"batch":{"id":%s,"step":%s,"k":%s,"n":%s,"here":%s,"elsewhere":%s,"next":%s,"far":%s' \
        "$ji" "$js" "${BT_K:-0}" "${BT_N:-0}" "$BT_HERE" "$BT_FARN" "$jn" "$jf"
    [ "$BT_LEFT" -ge 0 ] && printf ',"left":%s,"left_partial":%s' "$BT_LEFT" \
        "$([ "$BT_PARTIAL" = 1 ] && echo true || echo false)"
    printf '}'
}

show_json() {
    local f mode pid start label agent cmd elapsed sep="" st=idle jl ja jc jo i=0 free=0 eta_known=1 jleft
    local c csep sc
    prune
    batch_tail
    now
    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode _ _ _ _ _ < "$f"
        [ "$mode" = bench ] && st=bench || st=shared
        break
    done
    if [ "$st" = idle ]; then
        exec 7>"$DIR/rw"
        if flock -n -x 7 2>/dev/null; then
            flock -u 7
        else
            # A handover is not an orphan, and calling it one here took the machine out of
            # routing for as long as anyone kept asking.
            lock_unaccounted
            [ -n "$ORPH" ] && st=orphan || st=busy
        fi
        exec 7>&-
    fi
    # What a dispatcher needs to rank this machine. loadavg rather than the sum of what dibs
    # holds, because on a machine someone is working at most of the competing load was never
    # started through dibs and summing holders cannot see it. Scaled by 100, since this is
    # bash and the reader wants an integer.
    printf '{"t":%s,"state":"%s","cores":%s,"load":%s' \
        "$NOW" "$st" "$(nproc 2>/dev/null || echo 1)" \
        "$(load_x100)"
    # Which repos this machine has a build cache for. A dispatcher that does not know this
    # picks by load on the first run of a repo and lands somewhere with nothing cached, which
    # is minutes of cold compile chosen over seconds of queueing.
    printf ',"caches":['
    # Resolved here, not passed in: this half arrives over ssh, which forwards no environment.
    csep="" sc=${DIBS_SCRATCH:-$HOME/.cache/dibs}
    for c in "$sc/target"/*; do
        # A marker cargo writes, not the directory itself: preparing a worktree creates the
        # target directory whether or not anything is ever built in it, so without this every
        # machine claims every repo it has ever fetched.
        [ -f "$c/.rustc_info.json" ] || [ -d "$c/release" ] || [ -d "$c/debug" ] || continue
        printf '%s"%s"' "$csep" "$(basename "$c")"
        csep=","
    done
    printf ']'
    # Which repos a worktree can be prepared from at all. Separate from the cache above: a
    # cache makes a machine faster for a repo, a clone is what makes it possible, and a
    # dispatcher that cannot tell them apart sends work to a machine that fails at prepare.
    printf ',"clones":['
    lsep=""
    for c in "$HOME/prog"/*; do
        [ -d "$c/.git" ] || [ -f "$c/.git" ] || continue
        printf '%s"%s"' "$lsep" "$(basename "$c")"
        lsep=","
    done
    printf ']'
    printf ',"holders":['
    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent who dev cmd fp < "$f"
        elapsed=$(( NOW - start ))
        cpu_used "$pid"
        holder_output "$pid"
        idle_check "$pid" "$start"; [ -p "$DIR/hold.$pid" ] && IDLE_FOR=
        jstr jl "$label"; jstr ja "$agent"; jstr jc "$cmd"
        jstr jd "$dev"
        printf '%s{"mode":"%s","pid":%s,"label":%s,"agent":%s,"device":%s,"cmd":%s,"started":%s,"elapsed":%s,"cpu":%s' \
            "$sep" "$mode" "$pid" "$jl" "$ja" "$jd" "$jc" "$start" "$elapsed" "$CPU_USED"
        jleft=-1
        if estimate "$mode" "$label" "$agent" "$fp"; then
            remaining_of "$elapsed"
            [ "$REM" -gt "$free" ] && free=$REM
            jleft=$REM
            [ "$REM" -lt 0 ] && eta_known=0
            printf ',"est":%s,"est_lo":%s,"est_hi":%s,"est_n":%s,"est_scope":"%s"' \
                "$EST_V" "$EST_LO" "$EST_HI" "$EST_N" "$EST_SCOPE"
            est_wide && printf ',"est_wide":true'
            [ "$EST_OTHER" = 1 ] && printf ',"est_other_values":true'
            [ "$REM" -ge 0 ] && printf ',"remaining":%s,"remaining_kind":"%s"' "$REM" "$REM_KIND"
            overrunning "$elapsed" && printf ',"overrun":true'
        else
            eta_known=0
        fi
        holder_output "$pid"
        [ -n "$OUTPUT_FILE" ] && { jstr jo "$OUTPUT_FILE"; printf ',"output":%s' "$jo"; }
        [ "$CPU_RATE" -ge 0 ] && printf ',"cpu_rate":%s' "$CPU_RATE"
        [ -n "$IDLE_FOR" ] && printf ',"idle_for":%s,"idle_kind":"%s"' "$IDLE_FOR" "$IDLE_KIND"
        batch_json "$pid" "$jleft"
        printf '}'
        sep=","
    done
    printf '],"queue":['
    queue_sorted
    sep=""
    queue_eta_start "$free" "$eta_known"
    for f in "${QF[@]}"; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent who dev cmd fp < "$f"
        i=$((i+1))
        jstr jl "$label"; jstr ja "$agent"; jstr jc "$cmd"; jstr jd "$dev"
        printf '%s{"position":%s,"mode":"%s","pid":%s,"label":%s,"agent":%s,"device":%s,"cmd":%s,"arrived":%s,"waiting":%s' \
            "$sep" "$i" "$mode" "$pid" "$jl" "$ja" "$jd" "$jc" "$start" "$(( NOW - start ))"
        queue_eta "$mode" "$label" "$agent"
        [ "$QE_ETA" -ge 0 ] && printf ',"eta":%s' "$QE_ETA"
        jleft=-1
        [ "$QE_ETA" -ge 0 ] && estimate "$mode" "$label" "$agent" && [ "$EST_SCOPE" = this ] && jleft=$(( QE_ETA + EST_V ))
        batch_json "$pid" "$jleft"
        printf '}'
        sep=","
    done
    printf ']}\n'
}

show() {
    local f held=0 mode pid start label cmd elapsed rem free=0 total=0 eta_known=1 sig=
    local used el ue uh rr wa idl agent AC MC left usual depth=0 qnote= jleft
    if [ "$VERBOSE" = 1 ]; then
        echo "  [$DIR]"
        ls -la "$DIR" 2>&1 | sed 's/^/  /'
        echo "  [$HIST: $( [ -s "$HIST" ] && wc -l < "$HIST" || echo 0 ) runs recorded]"
    fi
    prune
    for f in "$DIR"/holder.* "$DIR"/waiting.*; do sig="$sig ${f##*/}"; done
    [ "$sig" = "$EST_SIG" ] || { EST=(); EST_SIG=$sig; }
    batch_tail
    for f in "$DIR"/waiting.*; do [ -e "$f" ] && depth=$((depth+1)); done
    [ "$depth" -gt 0 ] && qnote="$C_DIM ($depth queued)$C_OFF"

    for f in "$DIR"/holder.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent who dev cmd fp < "$f"
        [ "$dev" = - ] && dev=
        if [ "$held" -eq 0 ]; then
            [ "$mode" = bench ] && echo "${C_BUSY}dibs: BUSY, benchmark in progress${C_OFF}$qnote" \
                                || echo "${C_WARN}dibs: in use, shared${C_OFF}$qnote"
        fi
        held=$((held+1))
        now; elapsed=$(( NOW - start ))
        cpu_used "$pid"; used=$CPU_USED
        dur_ el "$elapsed"
        holder_output "$pid"
        idle_check "$pid" "$start"; [ -p "$DIR/hold.$pid" ] && IDLE_FOR=
        agent_hue "$agent"; mode_hue "$mode"
        if [ -n "$IDLE_FOR" ] && [ "$IDLE_FOR" -gt "${DIBS_IDLE_AFTER:-60}" ]; then
            dur_ idl "$IDLE_FOR"
            if [ "$IDLE_KIND" = never ]; then
                echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_WARN}[IDLE: no CPU at all in $idl, it is waiting on something]$C_OFF"
            else
                echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_WARN}[IDLE: ${used}s of CPU, none of it in the last $idl, it is waiting on something]$C_OFF"
            fi
            echo "    ${C_WARN}stop it with: dibs --kill $pid$C_OFF"
            echo "    $C_DIM$cmd$C_OFF"
            with_lines "$pid"
            echo "    ${C_DIM}from$C_OFF $AC$agent$C_OFF${dev:+${C_DIM} on $C_OFF$dev}"
            batch_lines "$pid" -1
            held=$((held+1))
            continue
        fi
        jleft=-1
        if estimate "$mode" "$label" "$agent" "$fp"; then
            remaining_of "$elapsed"
            [ "$REM" -gt "$free" ] && free=$REM
            jleft=$REM
            if [ "$REM" -lt 0 ]; then
                eta_known=0; left="longer than it has ever taken"
            elif [ "$REM_KIND" = bound ]; then
                dur_ rr "$REM"; left="under $rr left if it runs true to form"
            else
                dur_ rr "$REM"; left="~$rr left"
            fi
            if est_wide; then
                dur_ ue "$EST_LO"; dur_ uh "$EST_HI"
                [ "$EST_LO" -eq 0 ] && ue="under a second"
                usual="anywhere from $ue to $uh over $EST_N runs"
            elif [ "$EST_V" -eq 0 ]; then
                usual="under a second over $EST_N runs"; left=
            elif [ "$EST_N" = 1 ]; then
                # One sample is a fact about one run, not a habit, and saying "usually" of it
                # claims a regularity nothing has been observed to have.
                dur_ ue "$EST_V"; usual="ran once, in $ue"
            else
                dur_ ue "$EST_V"; usual="usually $ue over $EST_N runs"
            fi
            # A colon rather than a verb: the phrase after it has to read the same whether it
            # says "usually 5m00s" or "anywhere from 0s to 4m39s".
            [ "$EST_SCOPE" = agent ] && usual="nothing on this one; this agent's other $mode jobs: $usual"
            [ "$EST_SCOPE" = mode ] && usual="nothing on this one; every $mode job on the machine: $usual"
            [ "$EST_OTHER" = 1 ] && [ "$EST_SCOPE" = this ] && usual="nothing with these values; with others: $usual"
            if overrunning "$elapsed"; then
                dur_ uh "$EST_HI"
                echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_WARN}[STUCK? its slowest run was $uh, this is over twice that]$C_OFF"
                echo "    ${C_WARN}stop it with: dibs --kill $pid$C_OFF"
            else
                echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_DIM}[$usual${left:+, $left}]$C_OFF"
            fi
        else
            # Without the holder's typical duration there is no honest ETA for the queue.
            eta_known=0
            echo "  $MC$mode$C_OFF  $AC$label$C_OFF  $C_B$el$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_DIM}[no history for this one yet]$C_OFF"
        fi
        echo "    $C_DIM$cmd$C_OFF"
        with_lines "$pid"
        echo "    ${C_DIM}from$C_OFF $AC$agent$C_OFF${dev:+${C_DIM} on $C_OFF$dev}"
        batch_lines "$pid" "$jleft"
        # Where its output is going, so --out is discovered by reading the status rather than
        # by already knowing the feature exists. Silent when a job redirected nowhere, because
        # there is then nothing to offer and a line saying so would be noise on every tick.
        holder_output "$pid"
        [ -n "$OUTPUT_FILE" ] && \
            echo "    ${C_DIM}writing $OUTPUT_FILE$C_OFF   ${C_DIM}(dibs --on $(hostname -s) --out $pid)$C_OFF"
    done
    if [ "$held" -eq 0 ]; then
        exec 7>"$DIR/rw"
        if flock -n -x 7 2>/dev/null; then
            flock -u 7
            echo "${C_FREE}dibs: idle${C_OFF}"
        else
            lock_unaccounted
            if [ -n "$ORPH" ]; then
                echo "${C_BUSY}dibs: LOCKED BY AN ORPHAN.${C_OFF} No holder record, but the lock is taken, so"
                echo "  something outlived its parent. Nothing can run until it goes."
                # Named here rather than left as an instruction. This already runs on the
                # machine, so telling someone to go and ask it themselves is asking them to do
                # the one thing this could have done for them, at the moment they are least
                # able to.
                echo "  holding it:"
                for p in $ORPH; do
                    ps -o pid=,etime=,user=,args= -p "$p" 2>/dev/null |
                        sed 's/^ */    /' | cut -c1-100
                done
                echo "  Stop it with: dibs --kill <pid> --anyone"
            elif [ "$OPENERS" = 1 ]; then
                echo "${C_BUSY}dibs: busy${C_OFF}, a client has just taken the lock and is recording it."
            else
                echo "${C_BUSY}dibs: the lock is taken and nothing here reports holding it.${C_OFF}"
                echo "  If fuser is missing, install psmisc; without it an orphan cannot be named."
            fi
        fi
        exec 7>&-
    fi

    queue_sorted
    total=${#QF[@]}
    [ "$total" -eq 0 ] && return 0

    local i=0
    queue_eta_start "$free" "$eta_known"
    for f in "${QF[@]}"; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r mode pid start label agent who dev cmd fp < "$f"
        [ "$dev" = - ] && dev=
        i=$((i+1))
        age_ wa "$start"
        agent_hue "$agent"; mode_hue "$mode"
        queue_eta "$mode" "$label" "$agent"
        jleft=-1
        [ "$QE_ETA" -ge 0 ] && estimate "$mode" "$label" "$agent" && [ "$EST_SCOPE" = this ] && jleft=$(( QE_ETA + EST_V ))
        if [ "$QE_ETA" -gt 0 ]; then
            dur_ rr "$QE_ETA"
            echo "  ${C_Q}queued $i of $total$C_OFF: $MC$mode$C_OFF  $AC$label$C_OFF  waiting $C_B$wa$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_DIM}[~$rr until it starts]$C_OFF"
        elif [ "$QE_ETA" = 0 ]; then
            echo "  ${C_Q}queued $i of $total$C_OFF: $MC$mode$C_OFF  $AC$label$C_OFF  waiting $C_B$wa$C_OFF  ${C_DIM}pid $pid$C_OFF   ${C_DIM}[starts as soon as the lock frees]$C_OFF"
        else
            echo "  ${C_Q}queued $i of $total$C_OFF: $MC$mode$C_OFF  $AC$label$C_OFF  waiting $C_B$wa$C_OFF  ${C_DIM}pid $pid$C_OFF"
        fi
        echo "    $C_DIM$cmd$C_OFF"
        echo "    ${C_DIM}from$C_OFF $AC$agent$C_OFF${dev:+${C_DIM} on $C_OFF$dev}"
        batch_lines "$pid" "$jleft"
    done
    [ "$total" -gt 1 ] && echo "  $C_DIM(queued in arrival order; the kernel picks the actual wake order)$C_OFF"
    return 0
}

case "$MODE" in
    status)  [ "$JSON" = 1 ] && { show_json; exit 0; }
             show; exit 0 ;;
    watch)
        # No lock, like --status, and no work between ticks: the read is both the interval
        # and the liveness check, since EOF on the channel is how this side hears that the
        # terminal watching it is gone.
        exec 5<&0
        now; HEARD=$NOW
        while :; do
            if [ "$JSON" = 1 ]; then
                show_json
            else
            [ "$TTY" = 1 ] && printf '\033[H\033[2J\033[3J'
            printf '%s   every %ss, ctrl-c to stop\n' "$(date '+%H:%M:%S')" "$LABEL"
            show
            fi
            if [ "$NO_WATCH" = 1 ]; then
                sleep "$LABEL"
            else
                # The interval is the tick. A word from the caller only says it is still there,
                # and redrawing on one would cost the machine a render every time it spoke and
                # give a reader ticks that are not the interval it asked for.
                now; due=$(( NOW + LABEL ))
                while [ "$NOW" -lt "$due" ]; do
                    read -r -t "$(( due - NOW ))" -u 5 _
                    rc=$?
                    now
                    if [ "$rc" = 0 ]; then HEARD=$NOW
                    elif [ "$rc" -le 128 ]; then exit 0
                    elif [ "$LEASE" -gt 0 ] && [ $(( NOW - HEARD )) -gt "$LEASE" ]; then exit 0
                    fi
                done
            fi
        done ;;
    log)     n=$LABEL
             [ -s "$LOG" ] || { echo "Nothing logged yet ($LOG)."; exit 0; }
             printf '%-19s %-9s %-7s %-14s %6s %6s %5s  %-21s  %-22s %s\n' \
                 WHEN EVENT MODE LABEL QUEUED RAN EXIT JOB AGENT COMMAND
             # Lines written before events carried a job id name the process instead.
             tail -n "$n" "$LOG" | awk -F'\t' '{
                 printf "%-19s %-9s %-7s %-14s %6s %6s %5s  %-21s  %-22s %s%s\n",
                        substr($1,1,19), $2, $4, substr($5,1,14), $6, $7, $8,
                        (NF >= 12 && $12 != "-" ? $12 : "pid " $3),
                        substr(($10 == "" ? "?" : $10),1,22), substr($9,1,50),
                        (NF >= 11 && $11 != "-" ? "  [batch " $11 "]" : "")
             }'
             exit 0 ;;
    # What decides whether a binary built here can run somewhere else. Reported as facts
    # rather than as one hash: compatibility is directional, and a hash can only say that two
    # machines differ, never which of them can accept the other's work.
    abi)
        printf 'kernel %s\n' "$(uname -s)"
        printf 'arch %s\n' "$(uname -m)"
        abi_facts
        command -v rustc >/dev/null 2>&1 && printf 'rustc %s\n' "$(rustc -V 2>/dev/null | awk '{print $2}')"
        nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null |
            head -1 | awk 'NF { print "nvidia " $1 }'
        printf 'os %s\n' "$(os_name)"
        exit 0 ;;

    check)
        # Getting here at all has already proved the parts that cannot be tested directly: ssh
        # reached the machine, its login shell parsed the bootstrap line, and bash ran what
        # arrived. What follows is everything that would otherwise fail later and further away.
        FAIL=0 WARN=0
        ok()   { printf '  ok    %s\n' "$1"; }
        warn() { printf '  warn  %s\n' "$1"; WARN=$((WARN+1)); }
        bad()  { printf '  FAIL  %s\n' "$1"; FAIL=$((FAIL+1)); }
        note() { printf '        %s\n' "$1"; }

        echo "dibs --check on $(hostname -s)"
        echo

        ok "bash ${BASH_VERSION%%(*}, and the login shell parsed the bootstrap"

        missing=""
        for t in flock timeout; do command -v "$t" >/dev/null 2>&1 || missing="$missing $t"; done
        [ -z "$missing" ] && ok "flock and timeout present" || bad "missing, and nothing works without them:$missing"
        rsync_v=$(rsync --version 2>/dev/null | awk 'NR == 1 && $1 == "rsync" {print $3}')
        case "$rsync_v" in
            [3-9]*) ok "rsync $rsync_v, so a tree can be sent here" ;;
            *) bad "no rsync 3, so a tree sent from another computer cannot arrive"
               note "openrsync, which macOS ships as rsync, takes too few of its options." ;;
        esac
        if command -v setpriv >/dev/null 2>&1; then ok "setpriv present, so a job dies with its caller"
        else warn "no setpriv: a job whose caller is killed outright can outlive it"; fi

        # Without it a job whose work runs in short-lived children reads as idle and gets
        # reported stuck.
        if counts_reaped_children; then
            ok "the CPU of reaped children is counted, so idle detection sees a job's whole tree"
        else
            warn "the CPU of reaped children is not counted here: a working job may be misreported as idle"
        fi

        case "$LOCK_SCOPE" in
            shared) ok "lock directory is shared: $DIR" ;;
            explicit) warn "lock directory set explicitly: $DIR (only what set it will agree)" ;;
            *)
                warn "lock directory is keyed to this uid: $DIR"
                note "Anyone logging in as a different user takes a different lock, and both"
                note "are told the machine is idle. Fine if everyone shares this account, and"
                note "a wrong answer with nothing to notice it by if they do not."
                note ""
                shared_lock_howto ;;
        esac

        scratch=${DIBS_SCRATCH:-$HOME/.cache/dibs}
        fstype=$(df -PT "$scratch" 2>/dev/null | awk 'NR==2{print $2}')
        avail=$(df -Ph "$scratch" 2>/dev/null | awk 'NR==2{print $4}')
        if [ ! -w "$scratch" ]; then bad "scratch $scratch is not writable"
        elif [ "$fstype" = tmpfs ]; then
            bad "scratch $scratch is tmpfs, which is RAM and usually under a quota"
            note "One build tree there fills it for every user, and a full tmpfs breaks"
            note "every command including the ones for finding out why. Set DIBS_SCRATCH."
        else ok "scratch $scratch on $fstype, $avail free"; fi

        tdir=$scratch/target
        if [ -d "$tdir" ] && [ -w "$tdir" ]; then
            probe=$tdir/.dibs-reflink-check
            if echo x > "$probe" && cp --reflink=always "$probe" "$probe.copy" 2>/dev/null; then
                tfs=$(df -PT "$tdir/" 2>/dev/null | awk 'NR==2{print $2}')
                listed=$(timeout 60 du -sh "$tdir/" 2>/dev/null | cut -f1)
                ok "target directories on $tfs with reflinks: a new tree starts from a copy of its repo's latest"
                note "$(ls "$tdir" | wc -l) of them list ${listed:-more than 60s of du}, counting shared blocks once per copy."
                [ "$tfs" = xfs ] && note "df is no better a measure: XFS reserves about 2% of the disk up front for reflinks."
            else
                warn "no reflinks where target directories live, so every new tree builds from nothing"
                note "XFS or btrfs under $tdir lets a new tree start from a sibling's build for free."
            fi
            rm -f "$probe" "$probe.copy"
        fi

        case "$HIST" in
            /var/lib/dibs/*) ok "history and log shared: $(dirname "$HIST")" ;;
            *) warn "history is per-user: $HIST"
               note "Estimates are built only from your own runs, and the log shows only"
               note "your own jobs. Shared, as root:  install -d -m 2775 -g dibs /var/lib/dibs" ;;
        esac

        echo
        # Which repos this machine can prepare a worktree from. A machine with no clone of a
        # repo is dropped from that repo's routing entirely, so without this the loss is
        # invisible: the machine looks healthy, and simply never gets that work.
        repos=$(for d in "$HOME/prog"/*; do
                    [ -d "$d/.git" ] || [ -f "$d/.git" ] || continue
                    printf '%s ' "$(basename "$d")"
                done)
        if [ -n "$repos" ]; then
            echo "  repos it can build:  $repos"
        else
            warn "no clones under ~/prog, so no recipe can be run here at all"
            note "It is dropped from routing for every repo until one is cloned. Anything"
            note "this machine should build needs a clone at ~/prog/<repo> first."
        fi
        echo
        echo "  devices"
        cores=$(nproc 2>/dev/null)
        model=$(cpu_model)
        printf '    cpu   %s, %s threads\n' "${model:-unknown}" "${cores:-?}"
        gpu_report

        # Emitted from what was just detected, because a hand-written bus id is how an
        # inventory goes quietly stale. @NAME@ and @SSH@ are the client's to fill: a machine
        # cannot know the alias that reaches it.
        if [ "$LABEL" = check-write ]; then
            echo
            echo "--8<-- dibs inventory --8<--"
            printf '[machine.@NAME@]\n'
            printf 'ssh      = "@SSH@"\n'
            printf 'hostname = "%s"\n' "$(hostname -s 2>/dev/null || hostname)"
            printf 'probed   = "%s"\n' "$(date +%F)"
            # A battery means a laptop, and a laptop throttles, shares one memory pool
            # between CPU and iGPU, and moves. Absence of a GPU says nothing about this:
            # a CPU benchmark box is a machine worth measuring on.
            if has_battery; then
                printf 'measure  = false        # runs on a battery, so it throttles and moves\n'
                printf 'workstation = true      # someone works here; drop this if it is headless\n'
            fi
            printf '\n  [[machine.@NAME@.device]]\n'
            printf '  kind  = "cpu"\n'
            printf '  name  = "%s"\n' "${model:-unknown}"
            printf '  cores = %s\n' "${cores:-0}"
            gpu_entries
            echo "--8<-- end --8<--"
        fi

        echo
        if [ "$FAIL" -gt 0 ]; then
            echo "  $FAIL blocking, $WARN to look at. This machine is not ready."
            exit 1
        elif [ "$WARN" -gt 0 ]; then
            echo "  usable, $WARN thing(s) to look at."
        else
            echo "  ready."
        fi
        exit 0 ;;

    out)
        # A job's stdout is the ssh channel back to whoever started it, and nothing keeps a
        # copy. But agents overwhelmingly redirect into a file, and a redirect is an open
        # descriptor: the kernel will say where it points. So the output is not lost, it is
        # just somewhere nobody thought to look, and the tree walk that already exists for
        # CPU finds it.
        target=${LABEL%%.*} lines=${LABEL##*.} whole=0
        [ "$lines" = whole ] && { whole=1; lines=40; }
        # A job id names a directory, running or finished, so this works after the fact, which
        # is the case the process walk below cannot serve: a job that is gone has no fds.
        case "$target" in
            *-*)
                d=${DIBS_SCRATCH:-$HOME/.cache/dibs}/jobs/$target
                [ -f "$d/log" ] || { echo "no job $target under ${DIBS_SCRATCH:-$HOME/.cache/dibs}/jobs" >&2; exit 1; }
                # The whole log, for the caller to keep, once the job has finished. Capped, since
                # this takes no lock and a benchmark may be running beside it.
                if [ "$whole" = 1 ] && [ -f "$d/meta" ] && [ "$(wc -c < "$d/log")" -le 16777216 ]; then
                    echo DIBS-OUT-HEAD
                    echo "job $target  $(awk -F'\t' '$1=="mode"{m=$2} $1=="label"{l=$2} $1=="exit"{e=$2} $1=="ran"{r=$2} END{print m"  "l"  ran "r"s  exit "e}' "$d/meta")"
                    echo "  $(head -c 200 "$d/cmd" 2>/dev/null | tr '\n' ' ')"
                    echo "  $(hostname -s):$d/log"
                    echo DIBS-OUT-LOG
                    cat "$d/log"
                    exit 0
                fi
                if [ -f "$d/meta" ]; then
                    echo "job $target  $(awk -F'\t' '$1=="mode"{m=$2} $1=="label"{l=$2} $1=="exit"{e=$2} $1=="ran"{r=$2} END{print m"  "l"  ran "r"s  exit "e}' "$d/meta")"
                else
                    echo "job $target  still running"
                fi
                echo "  $(head -c 200 "$d/cmd" 2>/dev/null | tr '\n' ' ')"
                echo "  $d/log  ($(wc -c < "$d/log") bytes, last $lines lines)"
                tail -n "$lines" "$d/log" | sed 's/^/  | /'
                exit 0 ;;
        esac
        found=0
        for f in "$DIR"/holder.*; do
            [ -e "$f" ] || continue
            IFS=$'\t' read -r mode pid start label agent who dev cmd fp < "$f"
            [ "$target" = all ] || [ "$target" = "$pid" ] || continue
            found=1
            echo "$mode  $label  pid $pid  ($(age "$start"))"
            echo "  from $agent"
            # The root's own stdout is the channel it was launched down, not something the
            # job chose. Anything different from that is a redirect the job made itself.
            root=$(fd_path "$pid" 1)
            files=$(fd_targets "$pid" "$root")
            sink=$(ls -d "${DIBS_SCRATCH:-$HOME/.cache/dibs}"/jobs/*-"$pid"/log 2>/dev/null | head -1)
            if [ -z "$files" ] && [ -n "$sink" ]; then
                files=$sink
            fi
            if [ -z "$files" ]; then
                echo "  writing straight back to the agent that started it, so there is no"
                echo "  copy on disk to show. Only a job that redirects into a file, which is"
                echo "  ${C_DIM}cmd > \$DIBS_SCRATCH/x.log 2>&1, can be read from here.$C_OFF"
            else
                for t in $files; do
                    echo
                    echo "  $t  ($(wc -c < "$t" 2>/dev/null || echo 0) bytes, last $lines lines)"
                    tail -n "$lines" "$t" 2>/dev/null | sed 's/^/  | /'
                done
            fi
        done
        [ "$found" = 1 ] || {
            if [ "$target" = all ]; then echo "Nothing is running."
            else echo "Nothing holding the lock with pid $target." >&2; exit 1; fi
        }
        exit 0 ;;
    fetch)
        # No lock, like out, so what it sends is capped.
        d=${DIBS_SCRATCH:-$HOME/.cache/dibs}/jobs/$LABEL
        [ -d "$d" ] || { echo "no job $LABEL under ${DIBS_SCRATCH:-$HOME/.cache/dibs}/jobs" >&2; exit 1; }
        [ -d "$d/artifacts" ] || { echo "job $LABEL kept no files: its recipe names no artifacts, or it wrote none of them" >&2; exit 3; }
        size=$(du -sb "$d/artifacts" | cut -f1)
        if [ "$size" -gt 67108864 ]; then
            echo "job $LABEL kept $size bytes, more than a fetch without a lock takes. Copy them under the shared lock:" >&2
            echo "  dibs --sync -a :$d/artifacts/ ./" >&2
            exit 2
        fi
        echo DIBS-FETCH
        tar -C "$d/artifacts" -cf - . | base64
        exit 0 ;;
    kill|kill-force)
        # Takes no lock. The whole point is to work when the lock is what is broken.
        target=${LABEL%%.*} anyone=""
        [ "${LABEL#*.}" = any ] && anyone=1
        [[ $target =~ ^[0-9]{8}-[0-9]{6}-[0-9]+$ ]] && kill_batch_here "$target" "$anyone"
        found=""
        for f in "$DIR"/holder.* "$DIR"/waiting.*; do
            [ -e "$f" ] || continue
            [ "${f##*.}" = "$target" ] && found=$f
        done
        [ -n "$found" ] || { echo "Nothing holding or queued with pid $target." >&2; show >&2; exit 1; }
        IFS=$'\t' read -r mode pid start label agent who dev cmd fp < "$found"
        if ! still_the_same "$pid" "$found"; then
            rm -f "$found"
            echo "dibs: $mode $label ended without clearing its record, and pid $pid belongs to" >&2
            echo "  something else now. The record is gone and nothing was signalled." >&2
            exit 1
        fi
        # Whose job this is, checked before anything is signalled rather than reported after.
        # Several agents share this machine and read the same --status, so a pid copied out of
        # it belongs to whoever happens to be there; stopping someone else's measurement has
        # to be a thing you meant rather than a thing you did.
        # Keyed on the session rather than on the title it displays under, so a title that goes
        # stale or changes mid-run cannot make an agent a stranger to its own job. A record
        # written before this field existed has no id, and an unknown owner is not grounds to
        # refuse: it would strand exactly the jobs already running during an upgrade.
        if [ -n "$who" ] && [ -n "$AGENT_ID" ] && [ "$who" != "$AGENT_ID" ] && [ -z "$anyone" ]; then
            echo "dibs: $mode $label (pid $pid) belongs to $agent, not to you." >&2
            echo "  It has been running $(age "$start"). If it is stuck and in your way, or you" >&2
            echo "  know it should stop, say so:  dibs --kill $pid --anyone" >&2
            exit 2
        fi
        # Every shell of one account on one laptop has this same id, so a match proves nothing:
        # it is as likely another agent's job as this one's.
        case "$who" in
            shell-*)
                if [ -z "$anyone" ]; then
                    echo "dibs: $mode $label (pid $pid) was started by $who, which names an account, not a" >&2
                    echo "  session, so dibs cannot tell whether it is yours. If you know it is, or that it" >&2
                    echo "  should stop:  dibs --kill $pid --anyone" >&2
                    echo "  Export DIBS_AGENT once per session and your jobs can be told apart." >&2
                    exit 2
                fi ;;
        esac
        # Its descendants, never its process group. The holder is not its group's leader,
        # the pipeline that launched it is, so a group kill signals whatever else happens
        # to share that group rather than the job that was asked for.
        tree="$pid $(tree_below "$pid")"
        [ "$MODE" = kill-force ] && sig=KILL || sig=TERM
        signalled=""
        # Deepest first, so a parent cannot spawn more work while its children are dying.
        for victim in $(printf '%s\n' $tree | tac); do
            kill -"$sig" "$victim" 2>/dev/null && signalled="$signalled $victim"
        done
        if [ -n "$signalled" ]; then
            CMD_ONE="killed $mode $label (pid $pid) after $(age "$start"): $cmd"
            log_event killed
            echo "Sent SIG$sig to$signalled ($mode $label, held $(age "$start"))."
            echo "It belonged to $agent."
        else
            echo "Could not signal pid $pid; it may already be gone." >&2
        fi
        [ "$MODE" = kill ] && echo "If it survives that, run: dibs --kill $pid --force"
        exit 0 ;;
    release) prune; echo "Pruned dead entries."; reclaim; show
             echo "A live holder is a running command. Stop it with: kill <pid>"; exit 0 ;;
esac

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
    # The entry records the card a slot held when the machine was probed. A card pulled or
    # moved since leaves an address the Vulkan selectors below ignore without a word, and the
    # job would run on the first card under the name of the one asked for.
    if _holds=$(pci_chip "$DEV_PCI"); then
        if [ -z "$_holds" ] || { [ -n "$DEV_CHIP" ] && [ "$_holds" != "$(printf %s "$DEV_CHIP" | tr 'A-F' 'a-f')" ]; }; then
            echo "dibs: asked for $DEV_NAME, recorded as ${DEV_CHIP:-a card} in $DEV_PCI, and that slot now holds ${_holds:-nothing}." >&2
            echo "  Not running it on another card under that name." >&2
            echo "  Record what the machine holds now:  dibs --check $(hostname -s) --write" >&2
            exit 2
        fi
    fi
    # CUDA_VISIBLE_DEVICES takes an index or a GPU-<uuid>, and never a bus id. Handed one it
    # does not error: it ignores the value and leaves every device visible, so the job runs
    # on whatever is first and looks pinned. The UUID is what the bus id is translated into
    # here, at run time, from the slot the check above found still holding the recorded card.
    # Order-independent too, unlike an index.
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

START=$(date +%s)
printf -v JOB '%(%Y%m%d%H%M%S)T-%s' "$START" "$$"
# One record is one line. A multi-line command would otherwise be counted once per line
# and write a file the size of the script it is running.
CMD_ONE=$(printf %s "$CMD" | tr '\n\t' '  ' | cut -c1-200)
[ "${#CMD}" -gt 200 ] && CMD_ONE="$CMD_ONE …"
[ "$HOLD" = 1 ] && CMD_ONE="held for a command run elsewhere: $CMD_ONE"
# Deleting gigabytes is as much IO as writing them, so a sweep queues behind a measurement
# rather than competing with one, and the log carries what it reclaimed like any other job.
if [ "$MODE" = gc ]; then
    read -r GC_DAYS GC_DRY <<< "$CMD"
    CMD=$(gc_script "${GC_DAYS:-default}" "${GC_DRY:-0}")
    CMD_ONE="dibs --gc"
    [ "${GC_DAYS:-default}" = default ] || CMD_ONE="$CMD_ONE --days $GC_DAYS"
    [ "${GC_DRY:-0}" = 0 ] || CMD_ONE="$CMD_ONE --dry-run"
fi
# An end line even when the job is torn down, so the log never just stops mid-story.
# Nothing can be written if it is SIGKILLed, which is itself worth knowing when reading it.
# Every exit, not just the happy one: while the watch lives it holds the channel open and
# the caller's ssh cannot return, so giving up on a busy lock would hang the caller.
# Set before any record is written, or a signal in between leaves one behind.
WATCHDOG=""
trap 'rm -f "$DIR/waiting.$$" "$DIR/holder.$$" "$DIR/work.$$" "$DIR/cpu.$$" "$DIR/batch.$$" "$DIR/hold.$$" "$DIR/with.$$" "$0"
      [ "$WITH_UP" = 1 ] && kill -TERM "${WITH_PID[@]}" 2>/dev/null
      for p in ${PORT_NUM[*]:-}; do rm -f "$DIR/port.$p"; done
      [ -n "$WATCHDOG" ] && kill "$WATCHDOG" 2>/dev/null
      [ "$LOGGED_END" = 1 ] || log_event aborted' EXIT

# The fifo is also how --status tells a hold, which waits on purpose, from a job that is idle.
[ "$HOLD" = 1 ] && ! mkfifo "$DIR/hold.$$" && { echo "dibs: could not make $DIR/hold.$$, so nothing is held." >&2; exit 71; }
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$MODE" "$$" "$START" "$LABEL" "$AGENT" "$AGENT_ID" "${DEV_NAME:--}" "$CMD_ONE" "$FINGERPRINT" > "$DIR/waiting.$$"
[ -n "$BATCH" ] && printf '%s\n' "$BATCH" > "$DIR/batch.$$"
log_event arrived
# A cap nobody chose follows this job's own history, so work that always runs long is not killed
# at its mode's default, and the caller hears the cap now rather than as exit 124. Its own, down
# to the procedure where the recipe layer named one: a cap taken from a run of the same label that
# does a quarter of the work is how a legitimate run is killed at 124.
if [ "$MAXFROM" = default ] && [ "$HOLD" = 0 ] && [ "$MAXHOLD" -gt 0 ] && { [ "$MODE" = bench ] || [ "$MODE" = shared ]; } &&
    estimate "$MODE" "$LABEL" "$AGENT" "$FINGERPRINT" && [ "$EST_SCOPE" = this ] && [ "$EST_N" -ge 3 ] &&
    [ $(( EST_HI * 2 )) -gt "$MAXHOLD" ]; then
    dur_ CAP_WAS "$MAXHOLD"; dur_ CAP_P90 "$EST_HI"
    MAXHOLD=$(( EST_HI * 2 )); dur_ CAP_NOW "$MAXHOLD"
    echo "dibs: 90% of $EST_N runs of this took up to $CAP_P90, so it may hold the lock for $CAP_NOW rather than $CAP_WAS. --max sets it." >&2
fi
# stdin is the ssh channel, and nothing else reads it, so its EOF is how this side learns
# the caller is gone. tailscaled's ssh server does not turn a closed channel into a hangup,
# so waiting for one is not an option.
#
# It starts before the queue, not after it: a caller can die while its job is waiting, and
# a job whose caller is gone should leave the queue rather than hold a place for twenty
# minutes and then die the instant it is let in.
#
# A background job in a non-interactive shell has its stdin redirected from /dev/null, so
# the watch has to be handed the channel on another descriptor or it reads EOF at once and
# kills the job it is supposed to be protecting.
exec 5<&0
WORKFILE=$DIR/work.$$
MAIN=$$
caller_gone() {   # why
    local work="" p
    for p in $(cat "$WORKFILE" 2>/dev/null); do kill -0 "$p" 2>/dev/null && work="$work $p"; done
    CMD_ONE="$1: $CMD_ONE"
    log_event caller-gone
    if [ -n "$work" ]; then
        reap $work
    else
        kill -TERM "$MAIN" 2>/dev/null   # still queueing: stop waiting for a lock nobody wants
    fi
}
if [ "$NO_WATCH" != 1 ]; then
# A caller that is alive says something at least once a lease, and one that sleeps closes nothing,
# so silence counts as gone.
{ while :; do
      if [ "$LEASE" -gt 0 ]; then read -r -t "$LEASE" -u 5 line; else read -r -u 5 line; fi
      rc=$?
      [ "$rc" = 0 ] || break
      # A hold ends with its caller saying how the command went, which is not the caller going away.
      if [ "$HOLD" = 1 ] && [ "${line%% *}" = release ]; then
          st=${line#release }; case "$st" in ''|*[!0-9]*) st=1 ;; esac
          printf '%s\n' "$st" > "$DIR/hold.$MAIN"
          exit 0
      fi
  done 2>/dev/null
  if [ "$rc" -gt 128 ]; then caller_gone "caller silent for ${LEASE}s"; else caller_gone "caller gone"; fi
} &
WATCHDOG=$!
disown "$WATCHDOG" 2>/dev/null   # or bash announces "Terminated" when we tear it down
elif [ "$MODE" = rsh ]; then
# rsync owns stdin and reads none of it while a send prepares its tree, which can take minutes. The
# ssh session this script was exec'd from ends with the caller. Holding none of its streams, or the
# session would wait on this and this on the session.
{ trap 'kill $! 2>/dev/null; exit 0' TERM
  tail --pid="$PPID" -f /dev/null &
  wait $! && caller_gone "caller gone"
} </dev/null >/dev/null 2>&1 5<&- &
WATCHDOG=$!
disown "$WATCHDOG" 2>/dev/null
fi

# Three quarters of shared jobs here finish in under five seconds, and behind a queued
# benchmark every one of them was waiting up to twenty minutes. A quick one may go around,
# but only while the benchmark has been waiting less than DIBS_PATIENCE and only if its own
# history says it will be gone within DIBS_QUICK, so the most this can cost a benchmark is
# the two added together.
#
# Its own history, not its mode's: the median across all shared jobs is under a second, which
# would wave through the four minute ones as readily as the instant ones.
#
# None of this can spoil a measurement. It decides who waits, nothing else: once a benchmark
# holds the lock, flock refuses every shared caller whatever this returns.
may_bypass() {
    [ "$MODE" = shared ] && [ "${DIBS_BYPASS:-1}" = 1 ] || return 1
    local f m p st queued=0
    now
    for f in "$DIR"/waiting.*; do
        [ -e "$f" ] || continue
        IFS=$'\t' read -r m p st _ < "$f"
        [ "$m" = bench ] || continue
        alive "$p" || continue
        queued=1
        [ $(( NOW - st )) -lt "${DIBS_PATIENCE:-60}" ] || return 1
    done
    [ "$queued" = 1 ] || return 1
    estimate "$MODE" "$LABEL" "$AGENT" "$FINGERPRINT" || return 1
    [ "$EST_SCOPE" = this ] || return 1   # what its agent usually takes is not what it takes
    [ "$EST_V" -le "${DIBS_QUICK:-10}" ]
}

exec 9>"$DIR/gate"
exec 8>"$DIR/rw"

# Everyone passes through the gate, and an exclusive waiter keeps holding it while it waits
# for the real lock. Without that, a steady trickle of shared users starves benchmarks.
if may_bypass; then
    log_event bypassed
elif [ -n "$WAIT" ]; then
    flock -x -w "$WAIT" 9 || {
        echo "dibs: busy, a benchmark is queued ahead of you. Gave up after ${WAIT}s." >&2
        show >&2
        exit 75
    }
else
    flock -x 9
fi
if [ "$MODE" = bench ]; then FLAG=-x; else FLAG=-s; fi

# Say so the moment it is going to wait, rather than going quiet for twenty minutes. A
# caller that cannot afford the wait can act on this; one that goes quiet gets killed by
# its own timeout and leaves the work undone with nothing to show why.
if ! flock -n $FLAG 8 2>/dev/null; then
    queued_line >&2
    [ "$VERBOSE" = 1 ] && show >&2
else
    flock -u 8   # let the real acquisition below take it under the same rules
fi

if [ -n "$WAIT" ]; then
    flock $FLAG -w "$WAIT" 8 || {
        flock -u 9
        echo "dibs: still busy after ${WAIT}s, gave up." >&2
        show >&2
        exit 75
    }
else
    flock $FLAG 8
fi
flock -u 9

WAITED=$(( $(date +%s) - START ))
ACQUIRED=$(date +%s)
# Restamped, not just renamed: a holder's clock starts when it acquires. Carrying the
# arrival time over would have --status report a job as running for the time it spent
# queued, and every number drawn from that elapsed, the ETA, the stuck check and the idle
# check, would be measuring a stretch the job spent doing nothing because it was waiting.
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$MODE" "$$" "$ACQUIRED" "$LABEL" "$AGENT" "$AGENT_ID" "${DEV_NAME:--}" "$CMD_ONE" "$FINGERPRINT" > "$DIR/waiting.$$"
mv "$DIR/waiting.$$" "$DIR/holder.$$"
[ "$WAITED" -ge 5 ] && echo "dibs: acquired the $MODE lock after $(dur "$WAITED")" >&2

# 8>&- 9>&- so the workload does not inherit the lock descriptors. A child that outlives
# its parent would otherwise keep the lock held with no holder record to show for it, and
# every later caller would queue behind something --status swears is not there.
# A transport job's stdin is rsync's protocol stream and has to reach it. Backgrounding is
# what makes that awkward: a background job in a non-interactive shell has its stdin
# redirected from /dev/null, so it is handed the channel on the descriptor the watch would
# otherwise have used.
# Every job owns a directory under scratch holding what ran and everything it printed. The
# output used to go down the ssh channel and nowhere else, so the moment a caller cut it
# with tail it was gone; now the whole of it is on the machine, readable with --out after
# the job has finished, and the caller is told where. Both streams land in one file, in
# order, the way a build log is read. Old directories go with the worktrees' own age limit.
JOBDIR=$SCRATCH/jobs/$JOB
stay_awake $$
# Read with the lock held, just before the command, for the record a measurement keeps.
if [ "$MODE" = bench ]; then
    DIBS_STATE=$(machine_state)
    export DIBS_STATE
fi
JOBLOG=""
TEE=""
SINK=""
if [ "$MODE" != rsh ] && mkdir -p "$JOBDIR" 2>/dev/null; then
    printf '%s\n' "$CMD" > "$JOBDIR/cmd"
    export DIBS_JOB="$JOB"
    JOBLOG=$JOBDIR/log
    if [ "$STREAM" = 1 ]; then
        # Without the lock descriptors, or a tee outliving a killed job would hold the lock
        # with no holder record to show for it.
        mkfifo "$JOBDIR/pipe" 2>/dev/null && { tee "$JOBLOG" < "$JOBDIR/pipe" 8>&- 9>&- 5<&- & TEE=$!; } || JOBLOG=""
    else
        SINK=$JOBLOG
    fi
    find "$SCRATCH/jobs" -mindepth 1 -maxdepth 1 -mtime +"${DIBS_KEEP_DAYS:-14}" -exec rm -rf {} + 2>/dev/null 8>&- 9>&- 5<&- &
fi
RUN=$CMD
[ "$HOLD" = 1 ] && RUN="read -r st < $(printf %q "$DIR/hold.$$") && exit \"\$st\""
WITH_FAIL=""
if [ "${#PORT_NAME[@]}" -gt 0 ] && ! ports_take; then
    WITH_FAIL="no free port in ${DIBS_PORTS:-20500-20999} on $(hostname -s)"
    echo "dibs: $WITH_FAIL, so the command did not run." >&2
fi
[ -z "$WITH_FAIL" ] && [ "${#WITH_NAME[@]}" -gt 0 ] && { with_start; with_ready || with_stop; }
if [ -n "$WITH_FAIL" ]; then
    STATUS=77
    [ -n "$JOBLOG" ] && : >> "$JOBLOG"
    # A tee waiting for the job's output never gets a writer otherwise.
    [ -n "$TEE" ] && : > "$JOBDIR/pipe"
else
    if [ "$MODE" = rsh ]; then
        timeout --signal=TERM --kill-after=30 "$MAXHOLD" bash -c "$CMD" 8>&- 9>&- 0<&5 5<&- &
    elif [ -n "$JOBLOG" ] && [ "$MAXHOLD" -gt 0 ]; then
        timeout --signal=TERM --kill-after=30 "$MAXHOLD" bash -c "$RUN" 8>&- 9>&- 5<&- < /dev/null > "${SINK:-$JOBDIR/pipe}" 2>&1 &
    elif [ -n "$JOBLOG" ]; then
        bash -c "$RUN" 8>&- 9>&- 5<&- < /dev/null > "${SINK:-$JOBDIR/pipe}" 2>&1 &
    elif [ "$MAXHOLD" -gt 0 ]; then
        timeout --signal=TERM --kill-after=30 "$MAXHOLD" bash -c "$RUN" 8>&- 9>&- 5<&- < /dev/null &
    else
        bash -c "$RUN" 8>&- 9>&- 5<&- < /dev/null &
    fi
    WORK=$!
    echo "$WORK" >> "$WORKFILE"
    # --kill signals the whole tree, but a signal to this script alone would release the lock
    # with the job still running under it. Only from here: set while queueing, a trap would
    # wait for flock to return, and a job nobody wants would hold its place until let in.
    trap 'reap "$WORK"; exit 143' TERM
    trap 'reap "$WORK"; exit 129' HUP
    trap 'reap "$WORK"; exit 130' INT
    if [ "$HOLD" = 1 ]; then
        # The caller's command needs the ports too, and only this side knows what they are.
        ports=""
        for i in "${!PORT_NAME[@]}"; do ports="$ports ${PORT_NAME[$i]}=${PORT_NUM[$i]}"; done
        echo "DIBS-HOLDING$ports"
    fi
    if [ "$WITH_UP" = 1 ]; then
        wait -n -p ENDED "$WORK" "${WITH_PID[@]}"
        STATUS=$?
        if [ "$ENDED" != "$WORK" ]; then
            for i in "${!WITH_PID[@]}"; do
                [ "${WITH_PID[$i]}" = "$ENDED" ] &&
                    with_failed "$i" "exited $STATUS while the command ran" "the command was stopped"
            done
            reap "$WORK"
            wait "$WORK"
            STATUS=77
        fi
        with_stop
    else
        wait "$WORK"
        STATUS=$?
    fi
fi
[ -n "$BATCH_TAG" ] && [ -e "$DIR/cancelled.${BATCH_TAG%% *}" ] && STATUS=76
if [ -n "$TEE" ]; then
    wait "$TEE" 2>/dev/null
    rm -f "$JOBDIR/pipe"
fi
# It holds the channel open, and while it does the caller's ssh cannot return.
[ -n "$WATCHDOG" ] && kill "$WATCHDOG" 2>/dev/null
rm -f "$WORKFILE"
exec 5<&-
RUNTIME=$(( $(date +%s) - ACQUIRED ))
log_event finished "$WAITED" "$RUNTIME" "$STATUS"
LOGGED_END=1
# The one thing a caller gets that is the same shape every time: what ran, how long it
# waited and ran, how it ended and who ended it, and where the whole of its output is. A
# pipe on the caller's side cannot cut this off, since it is on stderr, and an exit code a
# filter replaced is still here.
if [ -n "$JOBLOG" ]; then
    by=command
    [ "$STATUS" -eq 124 ] && [ "$MAXHOLD" -gt 0 ] && by=dibs
    [ "$STATUS" -eq 77 ] && [ -n "$WITH_FAIL" ] && by=dibs
    [ "$STATUS" -eq 76 ] && [ -n "$BATCH_TAG" ] && [ -e "$DIR/cancelled.${BATCH_TAG%% *}" ] && by=dibs
    [ "$STATUS" -eq 78 ] && grep -qx DIBS-REFUSED "$JOBLOG" 2>/dev/null && by=dibs
    lines=$(wc -l < "$JOBLOG" 2>/dev/null || echo 0)
    # The digest: enough to see how it went, never so much that a caller has to cut it.
    if [ "$STREAM" != 1 ]; then
        head=${DIBS_DIGEST_HEAD:-20} tail_=${DIBS_DIGEST_TAIL:-20}
        if [ "$lines" -le $(( head + tail_ + 5 )) ]; then
            cat "$JOBLOG"
        else
            head -n "$head" "$JOBLOG"
            printf '\n... %s lines omitted. The whole log:  dibs --on %s --out %s  (%s)\n\n' \
                "$(( lines - head - tail_ ))" "$(hostname -s)" "$JOB" "$JOBLOG"
            tail -n "$tail_" "$JOBLOG"
        fi
    fi
    # What cargo compiled, when it ran, because "Finished" above a benchmark with nothing
    # compiled is the sentence that says the numbers are for the previous binary. Read from the
    # log rather than the command, since what runs cargo may be a runner the command starts.
    built=""
    if grep -q '^ *Finished .*target(s) in ' "$JOBLOG" 2>/dev/null; then
        n=$(grep -c '^ *Compiling ' "$JOBLOG" 2>/dev/null); n=${n:-0}
        [ "$n" -gt 0 ] && built="  built=$n" || built="  built=nothing"
    fi
    printf 'job %s  %s  %s  queued %ss  ran %ss  exit %s  by=%s%s\n' \
        "$JOB" "$MODE" "$LABEL" "$WAITED" "$RUNTIME" "$STATUS" "$by" "$built" >&2
    # A hold's command printed where it ran, so the log here has nothing in it.
    [ "$HOLD" = 1 ] || printf '  log %s:%s  (%s lines)  dibs --on %s --out %s\n' "$(hostname -s)" "$JOBLOG" "$lines" "$(hostname -s)" "$JOB" >&2
    for i in "${!PORT_NAME[@]}"; do
        printf '  port %s: %s on %s\n' "${PORT_NAME[$i]}" "${PORT_NUM[$i]:-none free}" "$(hostname -s)" >&2
    done
    for i in "${!WITH_NAME[@]}"; do
        printf '  with %s: %s  log %s:%s\n' "${WITH_NAME[$i]}" "${WITH_END[$i]:-not started}" "$(hostname -s)" "${WITH_LOG[$i]}" >&2
    done
    [ "$built" = "  built=nothing" ] &&
        echo "  built nothing: cargo compiled 0 crates, so a measurement after this measures the previous binary." >&2
    # The same failing command re-run unchanged fails the same way, and the log shows it done
    # several times over. Said once, in the trailer, when the previous attempt is minutes old.
    if [ "$STATUS" -ne 0 ] && [ "$HOLD" = 0 ]; then
        prev=""
        # Only jobs inside the window are read at all: two weeks of them is thousands of
        # directories, and comparing each one made every failure take seconds to report.
        window=${DIBS_REPEAT_WINDOW:-900}
        while read -r d; do
            [ "$d" != "$JOBDIR" ] && [ -f "$d/meta" ] && cmp -s "$d/cmd" "$JOBDIR/cmd" || continue
            age=$(( $(date +%s) - $(stat -c %Y "$d/meta" 2>/dev/null || echo 0) ))
            [ "$age" -le "$window" ] || continue
            e=$(awk -F'\t' '$1=="exit"{print $2}' "$d/meta")
            [ "$e" != 0 ] && prev="${d##*/} exit $e, $(dur "$age") ago"
        done < <(find "$SCRATCH/jobs" -mindepth 1 -maxdepth 1 -type d -mmin -$(( window / 60 + 1 )) 2>/dev/null | sort)
        [ -n "$prev" ] && echo "  this exact command already failed here: job $prev. Unchanged, it failed the same way." >&2
    fi
    printf 'mode\t%s\nlabel\t%s\nqueued\t%s\nran\t%s\nexit\t%s\nby\t%s\nagent\t%s\nlines\t%s\n' \
        "$MODE" "$LABEL" "$WAITED" "$RUNTIME" "$STATUS" "$by" "$AGENT" "$lines" > "$JOBDIR/meta"
fi
if [ "$(wc -l < "$LOG" 2>/dev/null || echo 0)" -gt 20000 ]; then
    tail -10000 "$LOG" > "$LOG.tmp" && mv "$LOG.tmp" "$LOG"
fi
# Say what to do about it, not only what happened. A cold build of a large workspace can pass
# half an hour on its own, and the answer is almost always to run the same thing again: the
# target directory survives, so a compile picks up from the crates that finished rather than
# starting over. Raising --max is for the job that genuinely needs longer in one go.
if [ "$STATUS" -eq 124 ]; then
    echo "dibs: stopped after holding the lock for ${MAXHOLD}s, which is --max for a $MODE job." >&2
    echo "  Nothing is wrong with it; it was simply told to hold no longer than that." >&2
    [ "$HOLD" = 1 ] || {
        echo "  Run it again; a compile picks up from the crates that already finished, since the" >&2
        echo "  build cache outlives the job. Anything else starts over." >&2
    }
    echo "  If it truly needs one long run, say so:  --max $(( MAXHOLD * 2 ))" >&2
fi

# What the next caller's estimate is built from, so only runs that did what they set out
# to do belong in it. A benchmark that failed in a second, or was killed for overrunning,
# is not a typical duration: recording it teaches every later caller the wrong number.
if [ "$STATUS" -eq 0 ]; then
    printf '%s\t%s\t%s\t%s\t%s\n' "$MODE" "$LABEL" "$(( $(date +%s) - ACQUIRED ))" "$AGENT" "$FINGERPRINT" >> "$HIST"
    if [ "$(wc -l < "$HIST")" -gt 1000 ]; then
        tail -500 "$HIST" > "$HIST.tmp" && mv "$HIST.tmp" "$HIST"
    fi
fi
exit "$STATUS"
