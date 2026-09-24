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

    measure "$S"/target/*
    for d in "$S"/target/*; do
        [ -d "$d" ] || continue
        [ -e "$d/.dibs-used" ] || echo swept > "$d/.dibs-used"
        t=$(used "$d"); verdict=""
        if [ $(( (now - t) / 86400 )) -gt "$TKEEP" ]; then
            if [ "$DRY" = 1 ]; then verdict="   would remove"; would=$(( would + ${MB[$d]:-0} ))
            else rm -rf "$d"; verdict="   removed"; freed=$(( freed + ${MB[$d]:-0} )); fi
        fi
        row "${MB[$d]:-0}" "$([ -n "$verdict" ] && echo 1 || echo 0)" \
            "$(printf '    %-40s %7s  used %s%s' "${d#"$S"/}" "$(size "${MB[$d]:-0}")" "$(ago "$t")" "$verdict")"
    done
    rows_out "  build caches, removed after ${TKEEP} days unused"

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
    # A cache reflinked from a sibling shares its blocks, and du counts them in both, so the sum
    # can come out larger than the disk. df is the truth about what is free.
    fsk=$(df -k "$S" 2>/dev/null | awk 'NR == 2 {print $2}')
    [ -n "$fsk" ] && [ "$total" -gt "$fsk" ] &&
        echo "  more than the disk holds, because a cache reflinked from a sibling is counted in both"
    df -h "$S" 2>/dev/null | awk 'NR == 2 {printf "  %s free of %s on %s\n", $4, $2, $6}'
GCEND
}

