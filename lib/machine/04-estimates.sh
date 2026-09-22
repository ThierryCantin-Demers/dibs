estimate() {   # mode label agent [fingerprint]; sets EST_V EST_N EST_SCOPE, 1 with nothing to go on
    local key="$1/$2/${3-}/${4-}"
    [ -n "${EST[$key]+set}" ] || EST[$key]=$(estimate_compute "$1" "$2" "${3-}" "${4-}")
    [ -n "${EST[$key]}" ] || return 1
    read -r EST_LO EST_V EST_HI EST_N EST_SCOPE <<< "${EST[$key]}"
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
    local vals="" scope=this
    # The sharpest key first, where the caller knows it: the same procedure with the same values,
    # which is what this job is about to do. One recipe measured on two backends is one label and
    # two costs, and a median across both predicts neither; a step added to a suite is the same
    # again. Both change the fingerprint, and neither should change the label.
    #
    # It falls back to the label the moment the narrow key is empty, so a procedure that has never
    # run is estimated exactly as it was before this existed, and nothing splinters.
    [ -n "${4-}" ] &&
        vals=$(awk -F'\t' -v m="$1" -v l="$2" -v f="$4" '$1==m && $2==l && $5==f {print $3}' "$HIST")
    [ -n "$vals" ] ||
        vals=$(awk -F'\t' -v m="$1" -v l="$2" '$1==m && $2==l {print $3}' "$HIST")
    if [ -z "$vals" ] && [ -n "${3-}" ]; then
        vals=$(awk -F'\t' -v m="$1" -v a="$3" '$1==m && $4==a {print $3}' "$HIST")
        scope=agent
    fi
    if [ -z "$vals" ]; then
        vals=$(awk -F'\t' -v m="$1" '$1==m {print $3}' "$HIST")
        scope=mode
    fi
    [ -z "$vals" ] && return 1
    printf '%s\n' "$vals" | sort -n | awk -v s="$scope" '
        {a[NR]=$1}
        END {
            if (NR%2) m=a[(NR+1)/2]; else m=int((a[NR/2]+a[NR/2+1])/2)
            h=int(0.9*NR); if (h < 0.9*NR) h++   # nearest rank, so it never falls below the median
            if (h < 1) h=1
            l=int(0.1*NR); if (l < 1) l=1
            print a[l], m, a[h], NR, s
        }'
}

