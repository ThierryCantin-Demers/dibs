---
name: dibs
description: "How an agent runs work on machines shared through dibs, named as dibs::<rule-name>: its locks, urgent and low-priority work, calls, batches and check-ins, machines and cards, recipes, and reading trailers and exit codes. Use when you make any dibs call, or when work needs a build or benchmarking machine, this one included when dibs lists it."
---

# dibs

## Motivation

A measurement shares its machine with nothing, and every result names the code, the machine and
the card it ran on. The machines are shared by people and their agents, and each rule here exists
because a measurement was spoiled without it.

What an agent's dibs calls are, cited as `dibs::<rule-name>`, and how it runs work through them.

**Upstream skills:** none.

## Rule files

Each sits beside this file, in `plugin/skills/dibs/` of the dibs repository. Read the one the work
needs before its first call.

| When the work | Read |
| --- | --- |
| copies files, starts a server, is driven from outside the machine, only compiles, is urgent, or should be in nobody's way | `locks.md` |
| is more than one call, must outlive this computer, or checks in | `batches.md` |
| picks a machine or a card, or asks who holds one | `machines.md` |
| runs, compares, sweeps or writes a recipe | `recipes.md` |
| comes back, or a call exits non-zero | `results.md` |

## Concepts

- **Machine:** a computer dibs shares, listed by `dibs --machines`; the same calls work on the
  Machine itself, which locks locally.
- **Measuring Machine:** a Machine whose numbers mean something, which every Machine is unless it
  is marked `measure = false`.
- **Call:** one run of the `dibs` command.
- **Job:** what a Call runs on a Machine, named by its job id.
- **Shared Lock:** the lock several Jobs hold at once, taken by `dibs run` (or plain `dibs`) and
  by builds and tests.
- **Exclusive Lock:** the lock one Job holds with nothing beside it, shared work included, taken by
  `dibs run --bench`.
- **Measurement:** a Job whose time or numbers are a result.
- **Peek:** a Call that takes no lock and prints its command's output as it is, with no Job id
  and no Trailer, running beside a Measurement, which pays for it
  (`dibs --on <machine> --peek <command>`, for `ps`, `nvidia-smi`, `ls`, `tail`, `cat` of a small
  file, `git status`).
- **Scratch:** the directory `$DIBS_SCRATCH` names on a Machine, where `TMPDIR` points; dibs never
  removes a directory named there, removes what sits under `TMPDIR` unchanged for two weeks by default at the
  next Recipe's prepare, and removes `$DIBS_SCRATCH/out` only at a person's `dibs --gc`.
- **Card:** one GPU of a Machine, named by the alias `dibs --machines -v` lists.
- **Recipe:** a repo's named build, test or bench procedure, run by
  `dibs build|test|bench <repo>@<ref> <recipe>`.
- **Batch:** one Call that runs a list of Calls as its Steps (`dibs batch`).
- **Trailer:** the line a Call ends with on stderr, which a pipe on the agent's side cannot cut off:
  `job <id>  <mode>  <label>  queued Ns  ran Ns  exit N  by=command|dibs  built=N|nothing  tests=N`.
- **Label:** the name a kind of work's durations are filed under (`yield-sweep`).
- **Check-in:** exit 80 of a Call whose Job has run 30 minutes, the Job going on without it.

## Locks

### through-dibs: Every command run on a Machine is a Call, and no `ssh`, `scp` or `rsync` reaches a Machine around one.

### measurement-is-exclusive: A Measurement runs under the Exclusive Lock on a Measuring Machine, never on a laptop.

### build-is-its-own-job: A Measurement's build is a Job of its own under the Shared Lock, before the Measurement and never inside it.

### peek-costs-nothing: A Peek compiles, downloads, copies and searches nothing, and reads nothing larger than a small file.

### no-clock-wait: No command a Call sends and no tool call of an agent waits on a clock, and what waits for something blocks on a fifo.

### scratch-is-own: Everything a Job writes on a Machine is in a directory of the agent's own naming under Scratch, never under `/tmp`.

### urgent-is-asked: A Call is `--urgent` only on the person's yes to that Call, since it goes before the queue, and an urgent Measurement stops everyone's shared Jobs on the Machine until it ends.

### nothing-deleted: An agent deletes nothing on a Machine and works around no permission error, and tells the person it works for.

## Calls

### call-in-background: Every Call that runs a Job is launched in the background, and nothing watches it before it wakes the agent.

### work-is-one-call: A piece of work of several Calls is one Batch, which wakes the agent once.

### check-in-followed: Every Check-in is followed in the background with the `--follow` line it printed.

### machine-named: A Measurement, a Peek, a sync, a kill of a pid, a log and a gc name their Machine, and only shared work is placed by dibs.

### agent-named: A runtime that is neither Claude Code nor in a herdr pane runs `export DIBS_AGENT='<the work>'` once per session, as its own statement, before its first Call.

### card-named: A Measurement on a Machine with more than one Card names its Cards with `--device`.

### devices-left-to-dibs: No Call sets `CUDA_VISIBLE_DEVICES`, `ROCR_VISIBLE_DEVICES` or `HIP_VISIBLE_DEVICES`.

## Recipes

### recipe-runs-its-work: Work a Recipe covers runs as that Recipe.

### missing-recipe-written: Work no Recipe covers that will run again gets a Recipe of its own.

### bare-call-has-reason: A `dibs shell` or `dibs raw` carries a `--reason`, which `dibs gaps` counts.

## Results

### result-from-trailer: A result is read from the Trailer and `dibs out`, and no Call's output passes through `head`, `tail`, `grep` or a redirect.

### build-before-numbers: A hand-written Measurement after a build whose Trailer says `built=nothing` is a result only when the binary already there is the one meant, while a Recipe checks whose build its target holds by itself.

### test-count-checked: A test run whose Trailer says `tests=0`, or a count far below the one meant, proves nothing whatever its exit.

### label-names-work: A Label names a kind of work and is the same every time it runs, never one run of it.

### job-ids-reported: An agent's report names the job ids its work produced.

## When dibs says no

### person-told: Exits 69, 70, 71 and 72 are told to the person the agent works for, never worked around.

### update-between-calls: When dibs says a newer dibs or newer recipes are out, `dibs --update` runs before the next Call, with no Job and no Batch of the agent's still running.

### friction-recorded: Whatever got in the agent's way that is not a missing Recipe is recorded as it happens with `dibs --friction '<one line>'`.

## Running work

1. `dibs list <repo>` says what the repo's Recipes are and the values each takes. A Recipe that
   covers the work runs as `dibs build|test|bench <repo>@<ref> <recipe> --<name> <value>`, with
   `@local` for the working tree. When none covers it, write one (`recipes.md`); work that will
   never run again runs as `dibs shell <repo>@local --reason '<why>' -- '<command>'`, or as
   `dibs run` when it needs a server, with `--with`, `--port` and `--ready` (`locks.md`).
2. Name the Machine and the Card where `machine-named` and `card-named` ask for them, with
   `--on <machine>` and `--device <alias>`. Before a Recipe's Measurement worth keeping, run the
   same `dibs bench ... --dry-run` and read which Card it would use.
3. Launch the Call in the background: the Bash tool's `run_in_background` in Claude Code, an
   `exec_command` that yields in Codex. When a `dibs run` cannot wait, `--wait <seconds>` makes it
   give up with exit 75 rather than queue past that long.
4. When it wakes you, read a Batch's summary or a single Call's Trailer, and `results.md` for any
   exit but 0.

## Waiting on something

Block on a fifo the work writes to once it is ready:

```bash
mkfifo ready
# start the work, which writes a line to ready once it is ready
read -r _ < ready
```
