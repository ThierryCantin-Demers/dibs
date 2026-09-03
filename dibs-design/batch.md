# One submission, one wake, one summary

## Why this reverses a deferred decision

`decisions.md` defers a per-call step spec, and `agent-interface.md` says outright that
"batching to save agent turns is worth less than it looks, because a call launched in the
background already reports back on its own. The saving is one round trip."

That was computed against the wrong cost. An agent's cost is turns times context size, and a
background completion does not cost a round trip: it wakes the agent, which re-reads its whole
context, reads the output, and replies. One job is three turns at full context, not one.

Measured: a single session launched about a hundred jobs, each as its own background call, and
that alone was most of an 870M-token day. A hundred jobs is three hundred turns; the same work
as one batch is three. The saving is not one round trip per job, it is two hundred and
ninety-seven turns.

The lock model is not implicated. Submission is.

## What must not change

`agent-interface.md` refuses graphs for a specific reason, and it is still right: "a graph
needs state that outlives a step, which is the one thing the design refuses." The lock is an
open descriptor held by a live process, so a machine that loses power comes back unlocked and
there is nothing to clean up. A half-executed batch holding a reservation would need an owner,
a timeout and a reaper, which is a scheduler arriving through the side door.

So the load-bearing decision here is:

**A batch adds no state on any machine. The driver is a client-side process and it is the
owner.** Each step is an ordinary dibs invocation that the driver makes, exactly as an agent
would have made it. If the driver dies, its children die, every lock releases, and nothing is
left behind, which is the lifetime rule a single job already has. This is what requirement 6
asks for and it is also what makes a batch admissible at all.

Two consequences worth stating plainly:

- **A batch gets no reservation.** After a shared build releases, the measured step queues like
  anyone else. That is the one thing a spec could add and it is explicitly out of scope, since
  it is a scheduling change. `decisions.md` already records that if it is ever built it must be
  release-and-requeue under the gate, never a lock upgrade.
- **`--batch` with `--detach` is refused**, the same as `--bench --detach` is today, and for a
  sharper version of the same reason: a detached job runs a bare command on a machine, dibs is
  never installed on a machine, so a detached driver could not invoke a single step.

## The format is a list of dibs command lines

`decisions.md` says that if a spec is ever built "it is a flag, not a file format." Honour
that: a batch file is the commands you already write, one per line, with an optional attribute
prefix in brackets.

```
# reduce on two cards, then bring the logs home
[build]                    dibs --on bench1 --label reduce-build 'cargo build --release --bench reduce'
[m4070 after=build]        dibs --bench --on bench1 --device gpu:rtx4070tisuper --label reduce-4070 'cargo bench --bench reduce'
[m4080 after=build cont]   dibs --bench --on bench2 --device gpu:rtx4080 --label reduce-4080 'cargo bench --bench reduce'
[home after=m4070,m4080]   dibs --sync --on bench1 :$DIBS_SCRATCH/batch/$DIBS_BATCH ./results/
```

Everything after `]` is verbatim a dibs command line, so a failed step is rerun by copy-paste,
which is most of what an agent does with a failure. Blank lines and `#` comments are skipped.
`dibs --batch -` reads the list from stdin.

The attributes are the whole vocabulary:

| attribute | meaning |
|---|---|
| `name` | first bare word, the step's name; defaults to its index |
| `after=a,b` | this step waits for those steps |
| `cont` | a failure here does not stop the batch |

A line whose command is not `dibs` is refused. A batch is a list of dibs calls, and keeping it
that way is what stops it growing into a runner.

## Ordering, and the only concurrency allowed

Sequential by default, in file order. Two steps may overlap only when **both** hold:

1. neither transitively depends on the other through `after=`, and
2. they resolve to different machines.

Independent steps on the *same* machine still run in order. Overlapping them is the surprising
overlap requirement 2 warns against, and it is also unnecessary: two shared steps on one
machine already interleave at the lock if that is what you want, and two measured steps must
not.

The driver resolves each step's machine before it starts, using the resolution `--which`
already implements, so the concurrency decision is made once against the same answer the step
itself will reach. It parses only what it needs for that and for the summary: `--on`,
`--device`, `--label`, and which of `--bench`, `--peek` or shared the step takes. Everything
else is passed through untouched to the child.

## Output, and the one thing on stdout

Each step redirects into `$DIBS_SCRATCH/batch/<batch-id>/<step>.log` **on the machine that ran
it**. That is a per-machine path, not one place, which is exactly why requirement 5 wants a
fetch step: the logs come home because a step says so, not by magic.

The batch id is `<date>-<driver pid>`, the same shape a detached job already uses, and the
driver exports it to every step as `DIBS_BATCH` so a step can name its own directory.

This also makes `dibs --out` work per step for free, since `--out` finds a running job's output
by resolving the file it redirected into.

Stdout is one summary, printed at the end, and nothing else:

```
batch 20260903-591908  4 steps, 1 failed, 11m24s
name    machine  device               lock    wall     exit  log
build   bench1   -                    shared  3m41s       0  <scratch>/batch/20260903-591908/build.log
m4070   bench1   gpu:rtx4070tisuper   bench   4m02s       0  .../m4070.log
m4080   bench2   gpu:rtx4080          bench   6m50s       1  .../m4080.log
home    bench1   -                    shared    12s       0  .../home.log
```

An agent reads that in the turn it woke for, and pulls a log only for the step that failed or
for the numbers it came for. That is the whole point: the summary must be small enough to be
free and complete enough that a successful batch needs no follow-up call at all.

`--verbose` streams each step's output as it goes, for a person watching.

## A batch in --status, --log and --kill

Each step is an ordinary holder and already shows up. What is missing is only the linkage, and
it is added **beside** the holder record rather than inside it: a `batch.<pid>` file naming the
batch id, the step name, and its position. Appending fields to the holder record instead would
corrupt the last field for any reader that has not been updated, and holder records are written
by one client and read by another, which may be a different version.

- `--status` groups a machine's steps under their batch, with who submitted it, which step is
  running and how many remain.
- `--log` records `batch` alongside the existing arrival and outcome events, so a batch is
  reconstructible from the record the way concurrency already is.
- `--kill <batch-id>` kills that machine's current step. **The rest is cancelled by the
  existing failure rule**, since a killed step is a failed step and a failed step stops the
  batch. No new cancellation path, and nothing to reap.

A batch spanning machines cannot be killed entirely from one machine, and the summary header
names the driver's host and pid so it can be reached. That is a property of the driver being
the owner, not a gap to be closed with state on the machine.

## dibs-run pipelines

dibs-run already runs a recipe's steps in order, one dibs invocation per step, each with its
own lock, each redirected to a file, all under one run record. It is a single-machine batch
runner that exists. What it lacks is a way to say "these recipes, on these machines and cards,
then fetch."

A pipeline recipe is a list of recipes, not a new kind of step:

```toml
[pipeline.reduce-all-cards]
[[pipeline.reduce-all-cards.stage]]
recipe = "bench.reduce"
on     = "bench1"
device = "gpu:rtx4070tisuper"

[[pipeline.reduce-all-cards.stage]]
recipe = "bench.reduce"
on     = "bench2"
device = "gpu:rtx4080"

[[pipeline.reduce-all-cards.fetch]]
from = "$DIBS_SCRATCH/batch/$DIBS_BATCH"
to   = "./results/"
```

One invocation, one run record, with each stage's existing run record nested inside it, so
`dibs-run runs` can still answer "what was actually measured, at which commit, on which card"
per stage while the pipeline is one comparable event.

`--on-each gpu:nvidia`, which `agent-interface.md` already proposed as the bounded matrix for
this case, stays as the shorthand that needs no file: it expands to a pipeline of one recipe
over the cards a machine reports, which is the common case and the one nobody should have to
write a file for.

## What this deliberately does not do

- **No retries.** A failed step stops the batch and says so in the summary.
- **No reservation and no scheduling change.** Steps queue exactly as they do now.
- **No new lock kinds.** Every step takes one of the three that exist.
- **No general DAG.** `after=` expresses order within a list, and a list is not a graph. The
  moment the format can express something cargo cannot, it has stopped being a way to ask for
  hardware.
- **No same-machine parallelism.** Deferred, not refused: it needs a reason beyond "it would
  be faster", and the lock already provides it for shared work.

## What a long batch does and does not cost

A running batch is neither opaque nor unstoppable, and both of those follow from the driver
being the owner rather than being features added on top.

**It can be stopped at any point, cleanly.** Interrupting the session kills the driver, its
children die, every lock releases, and no step state is left anywhere to reap. Steps that have
already finished have already written their logs, so stopping early loses only the steps that
had not run.

**Its state is legible while it runs.** `--status` says which step is running and how many are
behind it; each finished step's log is already on the machine that ran it, and the running
step's is being written. Checking on a batch is a `--peek` of a tail, which costs nothing and
does not disturb a measurement.

So the residue is narrower than "cannot steer", and it is worth naming exactly, because it is
the thing that decides how big a batch should be. **The agent does not wake mid-batch, so it
will not notice on its own that step 2 made steps 3 through 40 pointless.** Correction becomes
something the person initiates rather than something that happens by itself, and a `cont` step
can keep spending machine time on work that an earlier result had already invalidated.

When the criterion is known in advance, the guard already exists and needs nothing new. A step
runs an arbitrary command, and a command that exits non-zero stops the batch by the rule that
is already there, so a step can check its own result and refuse to let the rest proceed:

```
[m4070 after=build] dibs --bench --on bench1 --device gpu:rtx4070tisuper --label reduce-4070 \
    'cargo bench --bench reduce | tee r.txt; grep -q "PASS" r.txt || exit 1'
```

That covers the case where the agent could have written down what it was looking for. It does
not cover the case where it would have looked and judged, and pretending otherwise is how a
list of commands becomes a runner with a predicate language. That case wants two batches.

That gives the sizing rule directly. A batch is right when its steps do not need judging as
they go: the same measurement across cards, a build and the run that depends on it, a sweep
whose points are all wanted. A batch is wrong when a later step should only happen if an
earlier one said something in particular, and that case wants two batches with a look in
between, which is three turns and then three more rather than one hundred and twenty.

## Open

- Whether the driver should tolerate losing a machine mid-batch, or stop. Stopping is the
  current answer because it is the failure rule, but "one card was busy" is not obviously the
  same kind of event as "the step failed."
- Whether `--batch` should refuse a list whose steps are all shared and all on one machine,
  since that is a shell script with extra ceremony.
