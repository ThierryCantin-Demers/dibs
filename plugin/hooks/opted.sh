#!/bin/sh
# `dibs hook <name>`, only when DIBS_AGENT_HOOKS names it: a refusal nobody asked for would break
# a workflow, so installing the plugin turns none of them on.
case ",${DIBS_AGENT_HOOKS:-}," in
*",$1,"*) ;;
*) exit 0 ;;
esac
command -v dibs >/dev/null 2>&1 || exit 0
exec dibs hook "$1"
