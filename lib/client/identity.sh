# The title changes, and today it is also what decides whether a job is yours. A title that
# went stale made an agent unable to recognise its own work, and one that changes mid-session
# would make it unable to stop it. So ownership keys on the session, which does not move, and
# only the display uses the name.
agent_id() {
    local sid=${CLAUDE_CODE_HOST_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}
    if [ -n "$sid" ]; then printf '%s' "$sid"
    elif [ -n "${DIBS_AGENT:-}" ]; then
        printf 'agent-%s@%s-%s' "${USER:-someone}" "${HOSTNAME%%.*}" "$(printf %s "$DIBS_AGENT" | cksum | cut -d' ' -f1)"
    else
        printf 'shell-%s@%s' "${USER:-someone}" "${HOSTNAME%%.*}"
    fi
}

# Who is asking. Needed by the queue, by every mode that leaves a record, and by a report
# of what got in the way.
agent_name() {
    # Said outright, which is the only thing that works for a runtime this does not know how
    # to ask. Every other branch below is a way of guessing what this states.
    [ -n "${DIBS_AGENT:-}" ] && { printf '%s' "${DIBS_AGENT:0:48}"; return; }
    local id=${CLAUDE_CODE_HOST_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}} f t=""
    if [ -z "$id" ]; then
        # Codex publishes no session id, and its single shell process serves every one of its
        # sessions at once, so nothing in the environment separates them. Its own index does
        # carry thread names, and picking the most recently written one was measured against
        # three live sessions whose last writes were fifty-five seconds apart: it would name
        # the wrong one exactly when several agents are working, which is when a name is worth
        # having. So the runtime is all this claims, and DIBS_AGENT is how a session says more.
        [ "${CODEX_SHELL:-}" = 1 ] && { printf '%s' "a Codex session"; return; }
        printf '%s' "${USER:-someone} at a shell"; return
    fi
    for f in "$HOME/.config/Claude/claude-code-sessions"/*/*/"$id.json"; do
        [ -r "$f" ] || continue
        if command -v jq >/dev/null 2>&1; then
            t=$(jq -r '.title // empty' "$f" 2>/dev/null)
        else
            t=$(grep -o '"title":"[^"]*"' "$f" 2>/dev/null | head -1 | cut -d'"' -f4)
        fi
        [ -n "$t" ] && break
    done
    [ -n "$t" ] || t="session ${id#local_}"
    printf '%s' "${t:0:48}"
}

# The recipe layer, installed beside this script rather than on PATH, so that dibs stays the
# one command anyone types.
dibs_core() {
    local c
    for c in "${DIBS_CORE:-}" "$(dirname "$0")/../libexec/dibs/bin/dibs-core" \
             "$HOME/.local/libexec/dibs/bin/dibs-core"; do
        [ -n "$c" ] && [ -x "$c" ] && { printf '%s\n' "$c"; return 0; }
    done
    return 1
}

# The commit of the clone this runs from; empty for a copy, which has no history to compare.
dibs_version() {
    git -C "$(dirname "$(readlink -f "$0")")/.." rev-parse --short HEAD 2>/dev/null
}

# An agent carries what it learned about flags and output for its whole session, so a change
# underneath it is said once, to that session, the first time it calls after the change.
version_notice() {   # [quiet]
    local seen=${DIBS_SEEN:-${XDG_STATE_HOME:-$HOME/.local/state}/dibs/seen} now was f n clone
    now=$(dibs_version); [ -n "$now" ] || return 0
    f=$seen/$(agent_id | tr -c 'A-Za-z0-9._-' _)
    was=$(cat "$f" 2>/dev/null)
    [ "$was" = "$now" ] && return 0
    mkdir -p "$seen" 2>/dev/null && printf '%s\n' "$now" > "$f" 2>/dev/null
    find "$seen" -type f -mtime +30 -delete 2>/dev/null
    [ -n "$was" ] && [ "${1:-}" != quiet ] || return 0
    clone=$(dirname "$(readlink -f "$0")")/..; clone=$(cd "$clone" && pwd)
    {
        echo "dibs changed since this session last ran it: $was -> $now"
        if git -C "$clone" merge-base --is-ancestor "$was" "$now" 2>/dev/null; then
            n=$(git -C "$clone" rev-list --count "$was..$now")
            git -C "$clone" log --oneline --no-decorate -10 "$was..$now" | sed 's/^/  /'
            [ "$n" -gt 10 ] && echo "  and $((n - 10)) more:  git -C $clone log $was..$now"
        fi
        echo "  Flags and output you remember may be wrong now. Read dibs --help, and"
        echo "  $clone/dibs-agent-rules.md for how it is meant to be used."
    } >&2
}

# One line about what got in the way, recorded where the next session will read it. The text
# goes in the environment rather than in the arguments, because a report about a flag starts
# with that flag and every parser between here and the file would take it as one.
report_friction() {   # text
    local core
    need --friction "$1"
    core=$(dibs_core) ||
        { echo "dibs: --friction needs the recipe layer, which is not installed. Run install.sh from the clone." >&2; exit 2; }
    export DIBS_FRICTION_TEXT=$1 DIBS_FRICTION_BY=$(agent_name) DIBS_FRICTION_AT=$(dibs_version)
    version_notice
    exec "$core" friction
}
