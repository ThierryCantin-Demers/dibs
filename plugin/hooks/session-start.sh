#!/bin/sh
cat <<'JSON'
{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": "dibs puts a lock in front of shared build and benchmarking machines (`dibs --machines` lists them). On those machines:\n- Run everything through dibs, never ssh, scp or rsync straight at a machine: an unlocked command spoils whoever is measuring.\n- Anything measured runs with `dibs run --bench` on a benchmarking machine, never on a laptop.\n- Launch dibs calls in the background and never poll them; put a sequence in one `dibs batch`.\n- Never sleep inside a command sent to a machine.\n- When dibs says a newer version is out, run `dibs --update` between calls.\nBefore your first dibs call, load the `dibs` skill: it holds the full rules, from locks and batches to recipes, cards and exit codes."
  }
}
JSON
