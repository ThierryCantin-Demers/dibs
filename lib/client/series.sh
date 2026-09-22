series_usable() { [ -f "$SERIES" ] && [ "$(head -1 "$SERIES" 2>/dev/null)" = "$SERIES_V" ]; }

# Keyed on where the job actually goes, not on what it was called. One machine reached once
# through an inventory name and once through DIBS_HOST is one machine, and keying on the name
# would record it twice.
series_here() { printf '%s' "${HOST:-${MACHINE:-${TARGET:-?}}}"; }

series_name() {
    local n
    while read -r n; do
        [ -n "$n" ] && [ "$(inv "$n" ssh)" = "$1" ] && { printf '%s' "$n"; return; }
    done < <(inv_names 2>/dev/null)
    printf '%s' "${1#*@}"
}

series_check() {
    local prev_m prev_d prev_a line here_m here_d elsewhere m n
    here_m=$(series_here)
    here_d=${DEVICE:-none}
    series_usable || return 0
    line=$(awk -F'\t' -v l="$LABEL" -v m="$here_m" '$1 == l && $2 == m {print; exit}' "$SERIES") || return 0
    if [ -z "$line" ]; then
        # A benchmark sent to the wrong machine, by a DIBS_ON left over from other work, starts
        # a series of its own without complaint, so the first run anywhere new says where the rest are.
        # The recipe layer asked before it built, so its own steps have been told already.
        [ "${DIBS_FROM_RUN:-0}" = 1 ] && [ "$PREFLIGHT" = 0 ] && return 0
        elsewhere=""
        while IFS=$'\t' read -r m n; do
            elsewhere="${elsewhere:+$elsewhere, }$(series_name "$m")${n:+ ($n runs)}"
        done < <(awk -F'\t' -v l="$LABEL" -v m="$here_m" 'NR > 1 && $1 == l && $2 != m {print $2 "\t" ($6 > 0 ? $6 : "")}' "$SERIES")
        [ -n "$elsewhere" ] && echo "dibs: first run of '$LABEL' on $(series_name "$here_m"); its series is on $elsewhere." >&2
        return 0
    fi
    IFS=$'\t' read -r _ prev_m prev_d prev_a _ <<EOF
$line
EOF
    [ "$prev_d" = "$here_d" ] && return 0
    echo "dibs: '$LABEL' has been measured on another card of $(series_name "$here_m")." >&2
    printf '  before:  %s%s\n' "$prev_d" "$([ -n "$prev_a" ] && printf ', by %s' "$prev_a")" >&2
    printf '  now:     %s\n' "$here_d" >&2
    echo "  Those are two histories, not one series, and a number from one cannot be" >&2
    echo "  compared against a number from the other. Use the card it was measured on, or" >&2
    echo "  start its series on this machine again deliberately:  --new-series" >&2
    return 1
}

series_record() {
    local tmp d here_m runs=0; d=$(dirname "$SERIES")
    here_m=$(series_here)
    mkdir -p "$d" 2>/dev/null || return 0
    if [ "$NEW_SERIES" != 1 ] && series_usable; then
        runs=$(awk -F'\t' -v l="$LABEL" -v m="$here_m" '$1 == l && $2 == m {print $6 + 0; exit}' "$SERIES")
    fi
    tmp=$(mktemp "$SERIES.XXXXXX" 2>/dev/null) || return 0
    printf '%s\n' "$SERIES_V" > "$tmp"
    series_usable && awk -F'\t' -v l="$LABEL" -v m="$here_m" 'NR > 1 && !($1 == l && $2 == m)' "$SERIES" >> "$tmp"
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$LABEL" "$here_m" "${DEVICE:-none}" \
        "$(agent_name)" "$(date +%s)" "$(( ${runs:-0} + 1 ))" >> "$tmp"
    mv "$tmp" "$SERIES" 2>/dev/null || rm -f "$tmp"
}

# --new-series rides on the run it was passed with, and a run that measured nothing must not
# claim the label any more than a first attempt may. Silently, that is indistinguishable from
# the flag being ignored: the next run is refused with the same "before" as before, and the
# reading that follows is that it has to be passed on every run forever, which turns the guard
# off for that label permanently.
series_stayed_put() {
    [ "$MODE" = bench ] && [ "$NEW_SERIES" = 1 ] && [ "$STATUS" -ne 0 ] || return 0
    echo "dibs: the command failed, so '$LABEL' did not start its series here again and is still filed as it was." >&2
    echo "  --new-series takes effect only when the run it is passed with succeeds. Pass it" >&2
    echo "  again with a run that works, once, rather than on every run from here on." >&2
}
