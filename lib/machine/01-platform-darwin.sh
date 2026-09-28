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
