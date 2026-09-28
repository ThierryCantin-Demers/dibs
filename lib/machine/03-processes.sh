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
