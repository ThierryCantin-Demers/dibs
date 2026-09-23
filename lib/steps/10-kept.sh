if [ "$MODE" = out ] && [[ ${OUTPID:-} == *-* ]]; then
    if [ "${DIBS_OUT_WHOLE:-0}" = 1 ]; then
        LABEL="$OUTPID.whole"
    else
        if [ ! -f "$KEPT/$OUTPID/log" ]; then
            raw=$(mktemp "${TMPDIR:-/tmp}/dibs-out.XXXXXX") || exit 1
            DIBS_OUT_WHOLE=1 "$0" "${ORIG_ARGS[@]}" > "$raw"; status=$?
            if [ "$(head -n 1 "$raw")" != DIBS-OUT-HEAD ] || ! mkdir -p "$KEPT/$OUTPID.part"; then
                cat "$raw"; rm -f "$raw"; exit "$status"
            fi
            sed -n '2,/^DIBS-OUT-LOG$/{/^DIBS-OUT-LOG$/!p}' "$raw" > "$KEPT/$OUTPID.part/head"
            sed '1,/^DIBS-OUT-LOG$/d' "$raw" > "$KEPT/$OUTPID.part/log"
            rm -f "$raw"
            # Moved in file by file, since the job's directory may already hold its fetched files.
            mkdir -p "$KEPT/$OUTPID" && mv -f "$KEPT/$OUTPID.part/head" "$KEPT/$OUTPID/head" &&
                mv -f "$KEPT/$OUTPID.part/log" "$KEPT/$OUTPID/log"
            rm -rf "$KEPT/$OUTPID.part"
        fi
        out_kept "$KEPT/$OUTPID"
        exit 0
    fi
fi

# Kept beside the job's log, so a later fetch, and a fetch from after the machine is gone, reads
# them here.
if [ "$MODE" = fetch ] && [ "${DIBS_FETCH_RAW:-0}" != 1 ]; then
    kept=$KEPT/$FETCHJOB/artifacts
    if [ ! -d "$kept" ]; then
        raw=$(mktemp "${TMPDIR:-/tmp}/dibs-fetch.XXXXXX") || exit 1
        DIBS_FETCH_RAW=1 "$0" "${ORIG_ARGS[@]}" > "$raw"; status=$?
        if [ "$(head -n 1 "$raw")" != DIBS-FETCH ]; then
            cat "$raw"; rm -f "$raw"; exit "$([ "$status" = 0 ] && echo 1 || echo "$status")"
        fi
        rm -rf "$kept.part"
        if ! { mkdir -p "$kept.part" && sed 1d "$raw" | base64 -d | tar -C "$kept.part" -xf - &&
               mv -T "$kept.part" "$kept"; }; then
            rm -rf "$kept.part" "$raw"
            echo "dibs: the files of job $FETCHJOB arrived but could not be unpacked into $kept" >&2
            exit 1
        fi
        rm -f "$raw"
    fi
    n=$(find "$kept" -type f | wc -l)
    echo "dibs: $n file(s) from job $FETCHJOB, kept in $kept"
    find "$kept" -type f -printf '  %P\n' | sort | head -n 20
    [ "$n" -le 20 ] || echo "  and $((n - 20)) more"
    if [ -n "$FETCHTO" ]; then
        mkdir -p "$FETCHTO" && cp -a "$kept"/. "$FETCHTO"/ || exit 1
        echo "dibs: copied into $FETCHTO"
    fi
    exit 0
fi
