#!/usr/bin/env python3
"""Build the handoff report. Sources are read from the installed files and escaped rather
than pasted, so the appendix cannot drift from what actually runs."""
import html, pathlib, re

ROOT = pathlib.Path(__file__).resolve().parent.parent
# The ssh hook is a Claude hook, so it installs from the dotfiles repo rather than this one.
DOTFILES = pathlib.Path.home() / "dotfiles"
HERE = pathlib.Path(__file__).parent
OUT = HERE / "remote-benchmark-lock.html"

FILES = {
    "DIBS": ROOT / "bin/dibs",
    "HOOK": DOTFILES / "home/dot_claude/hooks/executable_no-direct-machine-ssh.sh",
    "TEST": ROOT / "tests/dibs-test.sh",
    "LIVE": ROOT / "tests/dibs-live-test.sh",
}

SNIPS = {
"SHIP": r"""# Local: the remote half travels on the command line and lands in a temp file.
PAYLOAD=$(remote_script | base64 | tr -d '\n')
REMOTE_SCRIPT="/tmp/.dibs-run.$$.$(date +%s).sh"
ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" \
    "printf %s '$PAYLOAD' | base64 -d > '$REMOTE_SCRIPT'; \
     bash '$REMOTE_SCRIPT' '$MODE' '$LABEL' ..."

# Remote: it unlinks itself immediately. Unlinking a running script is safe, bash keeps
# the open inode, and the caller cannot clean it up because that command line is read by
# the login shell, which may not be bash.
trap 'rm -f "$0"' EXIT""",

"GATE": r"""exec 9>"$DIR/gate"
exec 8>"$DIR/rw"

flock -x 9                                    # everyone queues here, in arrival order
if [ "$MODE" = bench ]; then FLAG=-x; else FLAG=-s; fi
flock $FLAG 8                                 # exclusive holds it against every shared user
flock -u 9                                    # gate released only once the real lock is held""",

"WATCH": r"""# stdin is the ssh channel, and nothing else reads it, so its EOF is how this side learns
# the caller is gone.
#
# A background job in a non-interactive shell has its stdin redirected from /dev/null, so
# the watch has to be handed the channel on another descriptor or it reads EOF at once and
# kills the job it is supposed to be protecting.
exec 5<&0
if [ "$NO_WATCH" != 1 ]; then
{ read -r -u 5 _ 2>/dev/null   # returns when the channel closes; no child to reap
  work=$(cat "$WORKFILE" 2>/dev/null)
  if [ -n "$work" ] && kill -0 "$work" 2>/dev/null; then
      pkill -TERM -P "$work" 2>/dev/null
      kill -TERM "$work" 2>/dev/null
  else
      kill -TERM "$MAIN" 2>/dev/null   # still queueing: stop waiting for a lock nobody wants
  fi
} &
WATCHDOG=$!
disown "$WATCHDOG" 2>/dev/null   # or bash announces "Terminated" when we tear it down
fi""",

"TTY": r"""# No TTY on purpose. With one, every tool downstream believes it is interactive: git
# opens its pager and the job blocks forever on a keystroke nobody will type.
export GIT_PAGER=cat PAGER=cat GIT_TERMINAL_PROMPT=0 DEBIAN_FRONTEND=noninteractive""",

"FDS": r"""# 8>&- 9>&- so the workload does not inherit the lock descriptors. A child that outlives
# its parent would otherwise keep the lock held with no holder record to show for it, and
# every later caller would queue behind something --status swears is not there.
timeout --signal=TERM --kill-after=30 "$MAXHOLD" bash -c "$CMD" 8>&- 9>&- 5<&- < /dev/null &
WORK=$!
echo "$WORK" > "$WORKFILE"
wait "$WORK"''""".replace("''", ""),

"CPU": r"""read -r line 2>/dev/null < "/proc/$pid/stat"
rest=${line#*") "}                    # comm can hold spaces and parentheses
read -ra a <<< "$rest"
# utime stime cutime cstime, fields 14 to 17, counted from the end of comm.
# The last two are the whole point: they are where a reaped child's work went.
total=$(( total + a[11] + a[12] + a[13] + a[14] ))""",

"READ": r"""# Wrong. The file has no trailing newline, so read fills kids and then reports EOF,
# and this throws away the data it just read.
read -r kids < "$f" 2>/dev/null || continue

# Right.
kids=""; read -r kids 2>/dev/null < "$f\"""".replace('"$f\\""', '"$f"'),

"ORDER": r"""read -r line < "$f" 2>/dev/null      # the shell still prints "No such file or directory"
read -r line 2>/dev/null < "$f"      # suppression is in place before the open is attempted""",

"STATUS": r"""$ dibs --status
dibs: BUSY, benchmark in progress
  bench  gemm-sweep  1m55s  pid 401758   [usually 1m26s over 37 runs, ~0s left]
    cd "$DIBS_SCRATCH/ws/proj" && bash sweep.sh cases.tsv 3
    from an agent session
  queued 1 of 1: bench  roofline  waiting 1m25s  pid 402188   [~0s until it starts]
    ./memory_curve 2>&1 | tail -90
    from CubeCL write probe fidelity""",

"AGENT": r"""# Who is asking, in terms that lead back to a window. Agents are Claude Code sessions, and
# the desktop keeps each session's title in a file named after it, which is the only name
# for an agent the user ever sees. Falling back to the id still tells two of them apart.
agent_name() {
    local id=${CLAUDE_CODE_HOST_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}} f t=""
    [ -n "$id" ] || { printf '%s' "${USER:-someone} at a shell"; return; }
    for f in "$HOME/.config/Claude/claude-code-sessions"/*/*/"$id.json"; do
        [ -r "$f" ] || continue
        t=$(jq -r '.title // empty' "$f" 2>/dev/null)
        [ -n "$t" ] && break
    done
    [ -n "$t" ] || t="session ${id#local_}"
    printf '%s' "${t:0:48}"
}""",

"RULES": r"""## Dibs
- Anything measured runs on the benchmarking machine, never on this laptop: the laptop is an Intel CPU and iGPU
  sharing one pool of memory, so its benchmark numbers and GPU kernel timings are noise.
- Never `ssh` the machine directly. A hook blocks it. Several agents use that machine at once, and an
  unlocked command ruins whoever is benchmarking.
- `dibs <command>` for builds, tests and anything that does work: shared, several at once.
  `dibs --bench <command>` for anything timed: exclusive, and it excludes shared users too,
  because a compile running beside a benchmark contaminates it as surely as a second benchmark
  would. `dibs --status` says who holds it and who is queued, and never blocks. Each entry
  names the agent that started it, by the title of its session, so I can go and ask that
  agent what it was running.
- `dibs --peek <command>` takes no lock at all, so it runs *beside* whatever is being measured
  and its cost is charged to that benchmark. Use it only for things that are effectively free.
  When in doubt use the shared lock: queueing costs you nothing, and ruining someone's
  twenty-minute sweep costs them everything.
- Always launch it with the Bash tool's `run_in_background` parameter, and never poll it. This is
  not a style preference: the machine is often busy for twenty minutes or more, a queued job waits
  that long before it starts, and a foreground call dies of its own timeout first. When that
  happens the work simply never runs, and you will report a failure whose cause is invisible.
- Exit 69 means the machine is unreachable. Tell me, do whatever does not need the machine, and do
  not retry in a loop. Exit 75 means it was busy and you had passed `--wait`. Exit 124 means the
  command overran `--max` and was killed while holding the lock.""",

"FIXTURE": r"""# Burns a known amount of CPU in children it then reaps, and the same amount on any
# machine: a fixture sized in loop iterations is only a fixture on the laptop it was
# written on, and this one has to clear a threshold measured in seconds.
burner() { echo "for i in 1 2 3; do timeout 1.2 python3 -c 'while True: pass'; done
           printf 'x\n' > $S/f-$1
           $(hold "$2")"; }""",

"PRUNE": '# kill -0 fails with EPERM for another user\'s process, which is not the same as dead.\n'
         '# On Linux the question "does this process exist" has a direct answer.\n'
         '[ -d "/proc/$pid" ] || rm -f "$f"',

"SYNC": r"""fifo()  { rm -f "$S/f-$1"; mkfifo "$S/f-$1"; }
hold()  { echo "read -r _ < $S/f-$1"; }                              # a fixture that blocks
free()  { timeout 5 bash -c "printf 'go\n' > '$S/f-$1'" 2>/dev/null; } # release it
# Blocks until the fixture says it reached a point, at no CPU cost and with a bound.
sync_() { timeout 30 bash -c "read -r _ < '$S/f-$1'" 2>/dev/null; }""",
}


def esc(s):
    return html.escape(s, quote=False)


page = (HERE / "report-template.html").read_text()

for key, path in FILES.items():
    # The ssh hook is a Claude hook and lives with whoever's config installs it, so a checkout
    # without one still builds rather than failing on a file that was never this repo's.
    body = path.read_text().rstrip("\n") if path.exists() else "(not present in this checkout)"
    page = page.replace("{{SRC_%s}}" % key, esc(body))
    page = page.replace("{{LINES_%s}}" % key, str(len(body.splitlines())))

for key, body in SNIPS.items():
    block = '<div class="codewrap"><pre><code>%s</code></pre></div>' % esc(body.rstrip("\n"))
    page = page.replace("{{SNIP_%s}}" % key, block)

left = sorted(set(re.findall(r"\{\{[A-Z_]+\}\}", page)))
assert not left, "unfilled placeholders: %s" % left
OUT.write_text(page)
print("wrote %s, %.1f KB" % (OUT, len(page) / 1024))
