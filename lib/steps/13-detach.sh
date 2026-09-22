# --- running somewhere that does not sleep ---------------------------------------------
#
# A job is killed when its caller dies, on purpose. Agents run on laptops, so the caller is
# always something that closes. The fix is not to weaken that rule but to move the caller: the
# command runs on a machine that stays up, holding the job for its whole life, and this session
# only starts it. Everything below the caller is unchanged, which is the whole of the design
# and also its one sharp edge: this takes no lock, so a job that needs one is a dibs call
# written inside it, and the flock is taken from there by a caller that will still be alive
# when it is granted.
#
# There is no scheduler here and there does not need to be one. Contention is already settled
# by the lock on the machine being used; the only thing missing was somewhere for the caller to
# live. DIBS_QUEUE names that machine, and without it nothing here applies.
QUEUE=${DIBS_QUEUE:-}
JOBS_DIR=${DIBS_JOBS_DIR:-'$HOME/.local/state/dibs/jobs'}

if [ "$DETACH" = 1 ]; then
    # --on names where the work goes, and for a detached job the work is the caller.
    [ "$PINNED" = 1 ] && [ -n "$HOST" ] && QUEUE=$HOST
    [ -n "$QUEUE" ] || {
        echo "dibs: no DIBS_QUEUE, so there is nowhere for the job to outlive this session." >&2
        echo "  Set it to the machine that stays up:  DIBS_QUEUE=dibs@<box>" >&2
        echo "  Or name one for this job:  dibs --detach --on <machine> <command>" >&2
        exit 2; }
    # setsid so it survives this ssh channel closing, and its own log from the first
    # instant so that a job which dies immediately still says why.
    queue_run <<'REMOTE' "$(printf %s "$COMMAND" | base64 | tr -d '\n')" "$JOBS_DIR" "$LABEL" \
                          "$(agent_name | base64 | tr -d '\n')"
set -eu
b64=$1; jobs=$(eval echo "$2"); label=${3:-job}; who=${4:-}
id="$(date +%Y%m%d-%H%M%S)-$$"
d=$jobs/$id
mkdir -p "$d"
printf %s "$b64" | base64 -d > "$d/cmd"
printf '%s
' "$label" > "$d/label"
printf %s "$who" | base64 -d 2>/dev/null > "$d/agent" || : > "$d/agent"
date +%s > "$d/started"
setsid bash -c 'd=$1; trap '"'"'echo $? > "$d/status"'"'"' EXIT; bash "$d/cmd"' _ "$d" \
    > "$d/log" 2>&1 < /dev/null &
echo $! > "$d/pid"
echo "$id"
REMOTE
    exit $?
fi

case "$MODE" in
    jobs|job|cancel)
        # Submitting took --on and reading the result did not, so a job could be sent to a
        # machine that --jobs then refused to look at.
        [ "$PINNED" = 1 ] && [ -n "$HOST" ] && QUEUE=$HOST
        [ -n "$QUEUE" ] || {
            echo "dibs: no DIBS_QUEUE, and no --on naming the machine the job is on." >&2
            exit 2; }
        ;;&
    jobs)
        queue_run <<'REMOTE' "$JOBS_DIR"
set -u
jobs=$(eval echo "$1")
[ -d "$jobs" ] || { echo "nothing submitted yet"; exit 0; }
fmt='%-24s %-14s %-9s %-16s %s\n'
printf "$fmt" ID LABEL STATE WHO COMMAND
for d in "$jobs"/*/; do
    [ -d "$d" ] || continue
    pid=$(cat "$d/pid" 2>/dev/null)
    if [ -f "$d/status" ]; then st="exit $(cat "$d/status")"
    elif [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then st=running
    else st=gone; fi
    printf "$fmt" "$(basename "$d")" \
        "$(cut -c1-14 < "$d/label" 2>/dev/null)" "$st" \
        "$(cut -c1-16 < "$d/agent" 2>/dev/null)" \
        "$(head -c 50 "$d/cmd" 2>/dev/null | tr '\n' ' ')"
done
REMOTE
        exit $? ;;
    cancel)
        queue_run <<'REMOTE' "$JOBS_DIR" "$LABEL" "$(agent_name | base64 | tr -d '\n')" "$ANYONE"
set -u
jobs=$(eval echo "$1"); id=$2; me=$(printf %s "${3:-}" | base64 -d 2>/dev/null); anyone=${4:-0}
d=$jobs/$id
[ -d "$d" ] || { echo "no such job: $id. dibs --jobs lists what is here." >&2; exit 2; }
# The same rule the machines got, for the same reason: everyone here shares one account, so
# the account cannot say whose job this is and the record has to.
owner=$(cat "$d/agent" 2>/dev/null)
if [ -n "$owner" ] && [ -n "$me" ] && [ "$owner" != "$me" ] && [ "$anyone" != 1 ]; then
    echo "dibs: job $id belongs to $owner, not to you." >&2
    echo "  If you know it should stop, say so:  dibs --cancel $id --anyone" >&2
    exit 2
fi
[ ! -f "$d/status" ] || { echo "job $id already finished, exit $(cat "$d/status")"; exit 0; }
pid=$(cat "$d/pid" 2>/dev/null)
[ -n "$pid" ] || { echo "job $id recorded no pid" >&2; exit 2; }
kill -0 "$pid" 2>/dev/null || { echo "job $id is not running"; exit 0; }
# The whole group, because the job was started with setsid and is its leader. Killing only
# the leader would leave the ssh client alive and the far side still holding the lock; killing
# the group takes that client down, and the far side learns of it the way it always does,
# through the end of its liveness channel.
kill -TERM -"$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
echo "stopped $id"
REMOTE
        exit $? ;;
    job)
        queue_run <<'REMOTE' "$JOBS_DIR" "$LABEL" "$OUT_N"
set -u
jobs=$(eval echo "$1"); id=$2; n=${3:-40}
d=$jobs/$id
[ -d "$d" ] || { echo "no such job: $id" >&2; exit 2; }
echo "command: $(cat "$d/cmd")"
if [ -f "$d/status" ]; then echo "state:   finished, exit $(cat "$d/status")"
else echo "state:   running, pid $(cat "$d/pid" 2>/dev/null)"; fi
echo "--- last $n lines ---"
tail -n "$n" "$d/log" 2>/dev/null
REMOTE
        exit $? ;;
esac
