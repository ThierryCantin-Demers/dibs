#!/bin/sh
# `dibs hook <name>`, only when DIBS_AGENT_HOOKS names it: a refusal nobody asked for would break
# a workflow, so installing the plugin turns none of them on.
case ",${DIBS_AGENT_HOOKS:-}," in
*",$1,"*) ;;
*) exit 0 ;;
esac
if ! command -v dibs >/dev/null 2>&1; then
    echo "DIBS_AGENT_HOOKS names $1, but dibs is not on PATH, so its hook cannot run: put dibs on PATH, or take $1 out of DIBS_AGENT_HOOKS." >&2
    exit 2
fi
exec dibs hook "$1"
