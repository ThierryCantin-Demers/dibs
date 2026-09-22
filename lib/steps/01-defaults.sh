# The account, not the person. Agents land on an unprivileged user whose home is the whole of
# what they can damage. It has no sudo and is not in the docker group, which grants root
# without needing one, and which is why running them as a human account was the exposure.
HOST=${DIBS_HOST:-}
# The bare name, for recognising this machine and for looking it up in tailscale. Derived
# from HOST, which is right unless HOST is an ssh alias whose Host line names something else.
# Set DIBS_HOSTNAME to the name the machine answers to when it is.
TARGET=${DIBS_HOSTNAME:-${HOST##*@}}

# The inventory names machines and the ssh aliases that reach them, which are yours, so it
# lives beside the recipe overrides rather than in this repo, which is public.
#
# Two layers, the way recipes have. A shared registry is what everyone gets
# without writing anything out, because a workplace where each person hand-lists the same
# machines has one list per person and no two of them agree. The personal file is then what it
# should have been all along: additions and overrides, for a machine that is yours alone.
MACHINES=${DIBS_MACHINES:-${XDG_CONFIG_HOME:-$HOME/.config}/dibs/machines.toml}
REGISTRY=${DIBS_REGISTRY_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/dibs/registry.toml}
# Where the shared one is fetched from, as scp spells it: dibs@host:path. Unset means there is
# no shared layer, which is the single-user case and stays exactly as it was.
REGISTRY_FROM=${DIBS_REGISTRY:-}

# hostname -s so a machine recognises itself and locks locally instead of trying to ssh to
# itself. The same script is on every machine; only this decides which side it is on.
SELF=$(hostname -s 2>/dev/null || hostname)

# GNU timeout is not everywhere: macOS has none by default and coreutils installs it as
# gtimeout. A client that cannot bound a poll should still be able to dispatch, so this
# degrades to running unbounded rather than to not running.
if command -v timeout >/dev/null 2>&1; then
    bounded() { timeout "$@"; }
elif command -v gtimeout >/dev/null 2>&1; then
    bounded() { gtimeout "$@"; }
else
    bounded() { shift; "$@"; }
fi

MACHINE=""
PINNED=0
HOSTFROM=""
MEASURABLE=1
ROUTE=0
# DIBS_HOST, when an inventory of several machines means it no longer chooses one.
UNHEEDED=""

MODE=shared
LABEL=""
WAIT=""
MAXHOLD=""
VERBOSE=0
KILLPID=""
OUTPID=""
FETCHJOB=""
FETCHTO=""
OUT_N=${DIBS_OUT_LINES:-40}
FORCE=0
ANYONE=0
DRY=0
GC_DAYS=""
LOG_N=40
WATCH_N=5
JSON=0
WRITE=0
ALL=0
FORGET=""
JOBID=""
PREFER=""
REPO=""
DEVICE=""
DEV_PCI=""
DEV_RT=""
DEV_CHIP=""
DEV_TWINS=1
NEW_SERIES=0
PREFLIGHT=0
HOLD=0
WITH_NAME=()
WITH_READY=()
WITH_CMD=()
PORT_NAME=()
READY_WITHIN=300
# The whole stream is what a person at a terminal wants and what the recipe layer parses; an agent
# wants the bounded digest, since it cannot know whether a call prints 3 lines or 30000
# and would cut it at the pipe, taking the exit status with it.
STREAM=${DIBS_STREAM:-${DIBS_FROM_RUN:-0}}
DETACH=0
COMMAND=""
SYNC=()
