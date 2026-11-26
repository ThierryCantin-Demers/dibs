# Machines

Where work runs, on which Card, and who is there, cited as `dibs/machines::<rule-name>`.

**Upstream files:** none.

## Machines

- **Placement:** the Machine dibs picks for shared work that names none: the one holding the
  repo's build cache, else the least busy, which `dibs --pick -v` shows and explains; a Measurement
  is never placed, since its history keys on its Machine.

### machine-checked-first: A Machine is checked with `dibs --check <host>`, its warnings read beside its exit, before its first use, and `--write` records it.

### non-measuring-machine-measures-nothing: No Measurement runs on a Machine marked `measure = false`, which still serves builds and tests.

### work-stays-with-cache: A repo's work runs on the Machine holding its build cache, never on an idle one to finish sooner, since the Measurement that needs the build cannot follow it.

## Cards

- **Model Alias:** a Card alias naming a model, which takes a free Card of it, or for a Label
  already measured on the Machine the Card its Series used, waiting for that one when it is busy
  (`--device aipror9700`, two different ones as `--device aipror9700,aipror9700`).
- **Series:** the runs of one Label on one Machine, all on one Card; numbers from two Machines never
  compare.
- **Card Benchmarks:** a Machine set to `benchmarks = "card"`, where a Measurement that names its
  Cards runs beside Measurements on the others, builds and tests still wait for all of them, and a
  whole-Machine Measurement queued behind them lets them join until the first of them ends.

### model-over-slot: A Card is named by its Model Alias where the Machine's Cards are alike, and by its slot only to measure that slot.

### cards-in-runtime-order: A Job over several Cards names them all in `--device`, in the order the runtime numbers them, and only on CUDA or ROCm.

### series-keeps-its-card: Every run in a Series names the same Card, and a Label moved to another Card on purpose passes `--new-series`.

### other-series-read: The first run of a Label on a Machine is read for where its other Series are, since a forgotten `--on` shows there.

### plain-label-per-machine: A Label is the same on every Machine, with no suffix naming one.

### moved-card-told: A Card refused with exit 2 for leaving the slot its alias recorded is told to the person, since `dibs --check <machine> --write` rewrites the inventory everyone measures by.

## Status

- **Status:** what `dibs status` says without blocking: who holds each Machine and their job ids,
  who is queued and for roughly how long, and for a Batch's Step, which step of how many and how
  long the Batch has left there; `--on <machine>` narrows it, and `dibs --kill <pid>` and
  `dibs --log [n]` ignore the lock, so a stuck Machine can still be freed.

### duration-is-read: How long work will take is read from `dibs status`, never guessed, and the later Calls of a script not yet submitted are said to be outside its estimate.

### kill-is-own: `dibs --kill <pid>` stops only a Job of the agent's own, unless the person asked for `--anyone`.

### no-watch: No agent runs `dibs --watch`, which is for a person at a terminal and costs the Machine.
