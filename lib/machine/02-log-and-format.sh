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
CLK=$(getconf CLK_TCK 2>/dev/null || echo 100)
HAVE_CHILDREN=0
[ "${DIBS_NO_CHILDREN:-0}" = 1 ] || { [ -r "/proc/$$/task/$$/children" ] && HAVE_CHILDREN=1; }
