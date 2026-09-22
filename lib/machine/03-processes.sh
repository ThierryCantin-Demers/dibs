cpu_used() {   # sets CPU_TICKS and CPU_USED
    [ "$HAVE_CHILDREN" = 1 ] || {
        CPU_TICKS=$(cpu_used_scan "$1"); CPU_USED=$(( CPU_TICKS / CLK )); return; }
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
    CPU_USED=$(( total / CLK ))
}

# Kernels built without CONFIG_PROC_CHILDREN cannot be asked downward, so the tree has to be
# rebuilt from every process's parent.
cpu_used_scan() {
    local pids
    pids=$(ps -eo pid=,ppid= | awk -v root="$1" '
        {pid[NR]=$1; par[NR]=$2}
        END {
            want[root]=1
            do {
                added=0
                for (i=1; i<=NR; i++)
                    if (want[par[i]] && !want[pid[i]]) { want[pid[i]]=1; added=1 }
            } while (added)
            for (i=1; i<=NR; i++) if (want[pid[i]]) print pid[i]
        }')
    { for p in $pids; do cat "/proc/$p/stat" 2>/dev/null; done; } | awk '
        {
            rest = substr($0, index($0, ") ") + 2)   # comm can hold spaces; skip past it
            n = split(rest, a, " ")
            if (n >= 15) sum += a[12] + a[13] + a[14] + a[15]
        }
        END {print sum + 0}'
}

# Where a job's own output is going, walking its tree the way cpu_used does. Deeper first
# in the order they come back, because the redirect an agent wrote is always below the shells
# the wrapper puts in the way. A file is reported once however many descendants hold it open.
fd_targets() {   # root-pid channel; prints distinct regular files the tree writes to
    local -a q=("$1")
    local i=0 pid fd t f k seen=" " out=""
    while [ "$i" -lt "${#q[@]}" ]; do
        pid=${q[i]}; i=$((i+1))
        for fd in 1 2; do
            t=$(readlink "/proc/$pid/fd/$fd" 2>/dev/null) || continue
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
        for f in /proc/$pid/task/*/children; do
            [ -r "$f" ] || continue
            k=""; read -r k 2>/dev/null < "$f"
            for t in $k; do q+=("$t"); done
        done
    done
    printf '%s' "$out"
}

# The first file a job's tree is writing to, which is what --out would show. Separate from
# fd_targets' full list because the status wants one line, not an inventory.
holder_output() {   # pid; sets OUTPUT_FILE
    local root
    OUTPUT_FILE=
    root=$(readlink "/proc/$1/fd/1" 2>/dev/null)
    OUTPUT_FILE=$(fd_targets "$1" "$root" | grep -v '/with-[a-z0-9_]*\.log$' | head -1)
}

# A median moves only when a run finishes, and a run appends to the history before its
# holder file goes, so a redraw that finds the same entries in the lock directory is looking
# at the same history it computed against last time.
declare -A EST=()
EST_SIG=""
