# A narrower interface for agents

`dibs` today takes an arbitrary shell command. That is why it works at all, and it is also why
four separate problems keep recurring. This is a design for constraining it, and for finding
out where the constraint is wrong rather than assuming.

Nothing here is built.

## What the open interface actually costs

Each of these is measured on the real history, not anticipated.

**Estimates cannot work.** 51 of 80 label keys have been seen exactly once, because an agent
writing a command by hand names the run rather than the kind of work. A label used once files
its duration where nothing will ever look it up, which is why the queue could not say how long
anything would take until percentiles papered over it.

**Half the jobs cannot be watched.** `dibs --out` reads a job's output by finding the file it
redirected into. Agents that redirect get read; agents that do not are invisible. Nothing
decides which, so it is a coin flip per command.

**A shared quota was exhausted by one agent's build tree.** Agents chose their own scratch
paths, one of them chose `/tmp`, and 12.3G of cargo targets filled a tmpfs quota shared by
every agent, breaking every command on the machine including the ones for diagnosing it.

**The exclusive lock is used for compiling.** Two `cargo bench --no-run` runs held the whole
machine for 31.8 minutes doing something that tolerates neighbours perfectly, which is 17% of
all exclusive time ever recorded. The rule against it is prose in CLAUDE.md, and prose is
advisory.

The pattern is the same each time. A rule was written asking for discipline, and the interface
made the undisciplined thing equally easy. A verb that carries the right behaviour does not
need the rule.

## What the log says agents are actually doing

167 arrivals, six agents, read off the real log rather than imagined.

| what | how many |
|---|---|
| jobs naming a hand-written `~/worktrees/...` path | **107 of 179** |
| jobs chaining several commands with `&&` or `;` | **69 of 179**, 38% |
| commands whose first word is `bash`, or an inline `$'...'` shell program | **~75** |
| commands whose first word is `cargo` | 9 |
| jobs hand-rolling `git worktree add` | 5 |
| jobs copying a `target/` directory between worktrees with `mv` or `cp -a` | several |
| jobs touching `/tmp` | 21 |
| **jobs that redirect their output to a log file** | **2** |

The dominant shape is one long chained command that sets up or enters a worktree, arranges a
build cache, and then runs something in it. That is why the commands are long, why they are
written as inline shell programs, and why six agents have each invented their own version of
the same dance. Nobody chose this; it is what an interface taking one string invites.

Three things fall straight out of that table.

**The worktree dance is the missing verb, not the build.** Only nine commands start with
`cargo`. The rest of the length is getting to the point where cargo can be run at all, and each
agent rediscovers it, differently, including whether to `mv` or `cp -a` the target directory
from a sibling worktree. That copying is a build cache being managed by hand, which is also how
12.3G ended up somewhere it should not have been.

**`dibs --out` is nearly useless in practice and the log says exactly why.** It reads a job's
output by finding the file the job redirected into, and two jobs out of 179 redirected. A verb
that always redirects to a known path turns a feature that works occasionally into one that
works always.

**Six agents, six conventions.** The same worktree setup, the same target-dir juggling, the
same guesses about where scratch belongs, re-derived per session and inconsistently. A verb is
how that stops being rediscovered.

## The principle

**Constrain where wrong usage corrupts data or wastes the machine. Leave inspection open.**

`--peek` is free and read-only and should stay as unconstrained as it is: an agent looking at
`nvidia-smi`, `git status` or a log file cannot spoil anything. The narrow interface is for the
path that takes a lock and produces numbers, because that is where a wrong command silently
produces a wrong result rather than an error.

## The verbs

Every verb takes `<repo>@<ref>`, and dibs guarantees the worktree exists at that ref with a
build cache shared per repo. That one guarantee is what removes most of the length from the
commands in the log above.

```
dibs build <repo>@<ref> [cargo args]      shared lock
dibs test  <repo>@<ref> [filter]          shared lock
dibs bench <repo>@<ref> <suite>           exclusive, pinned, recorded
dibs shell <repo>@<ref> --reason "..."    an arbitrary command, in a prepared worktree
dibs raw   --reason "..." <command>       an arbitrary command, nothing prepared
```

`shell` matters as much as `bench`. Most of what agents do is neither a build nor a benchmark,
it is some ad-hoc command that nonetheless needs the worktree and the cache set up correctly.
Without it every one of those falls to `raw` and the worktree dance comes straight back.

alongside what already exists and does not change: `--status`, `--watch`, `--out`, `--log`,
`--peek`, `--kill`, `--sync`.

## Recipes live in the repo being measured

Not in a central configuration file here. `.dibs.toml` sits in burn, cubecl, cubek:

```toml
[bench.gemm-roofline]
needs     = "gpu:nvidia:tensor"
isolation = "machine"
command   = "cargo bench --bench roofline -- --save-baseline {baseline}"
```

The repo knows its own build and benchmark commands, they version with the code, and a
benchmark added in a pull request brings its recipe with it rather than needing a separate
change here. It is the same argument that keeps a roadmap in the repo it describes.

## What this buys by construction

- **Labels are derived**, `cubek/gemm-roofline`, so they are stable forever without anyone
  being asked to keep them stable. Estimates start working, and the label becomes a sound key
  for binding a benchmark to one device.
- **Output always lands in a known file**, so `dibs --out` works for every job rather than for
  the ones that happened to redirect.
- **Scratch paths are owned by dibs**, so the quota incident cannot recur by choice.
- **The build/measure split becomes structural.** `dibs build` takes the shared lock and
  `dibs bench` the exclusive one; a bench recipe whose command compiles is a mistake in a file
  that can be reviewed, rather than sixteen minutes of a shared machine nobody notices.
- **Isolation and hardware requirements come from the recipe**, not from an agent's judgement
  at the call site. This matters most for the multi-machine design next door, where getting
  `--needs` wrong routes a tensor-core benchmark to a card that has none.

## The escape hatch, and why it is instrumented rather than discouraged

```
dibs raw --reason "bisecting a segfault, need git bisect run across builds" <command>
```

The reason is required, and it is recorded in the log with the command. That makes the fallback
slightly costly, so a recipe is preferred where one exists, and it produces the thing actually
worth having: a reviewable list of what did not fit and why. `dibs --gaps` groups the reasons,
and a reason that keeps recurring is a specification for the next verb, written by the agents
that needed it rather than guessed at here.

**Raw usage is not a number to drive to zero.** A one-off sweep written for one investigation
is a benchmark that will never run again, and forcing a recipe for it is friction with no
payoff. If a third of calls stay raw forever while the repeated work sits in recipes, that is
the design working. The failure mode to watch for is the opposite: a recurring reason that
nobody promoted.

## The spec: steps belong in the recipe, not in the request

There are two different things that both look like "a DSL", and they have opposite properties.

**A spec submitted per call** — an agent hands dibs a JSON graph of steps at invocation time —
does nothing for traceability. It is exactly as opaque as `bash /tmp/tm-sweep.sh`, just with
more syntax: it exists only in that one invocation, nothing versions it, and three weeks later
the number in the history still cannot be traced to what produced it.

**Steps declared in the recipe**, in the repo, under git, are the opposite of opaque. That is
what makes the run reproducible: check out the ref, read the recipe, run it again.

So the multi-step form is worth having, and it belongs in the recipe file:

```toml
[bench.gemm-roofline]
needs     = "gpu, num_tensor_cores >= 1"
isolation = "machine"

  [[bench.gemm-roofline.step]]
  lock = "shared"                                  # compiling tolerates neighbours
  run  = "cargo build --release --bench roofline"

  [[bench.gemm-roofline.step]]
  lock = "exclusive"                               # only this is measured
  run  = "cargo bench --bench roofline"
```

This is the point that the earlier reasoning here missed. **The reason agents write opaque
scripts is that a script is the only way to express several steps at all**, and 36 of the 38
exclusive jobs in the log are exactly that. Given no legible form for multi-step work, they
reach for the one form that exists. A recipe with steps gives that shape a home where it is
versioned, reviewable, and visible in the log as `cubek@ref gemm-roofline` rather than as a
path in `/tmp` that no longer exists.

It also removes the last excuse for compiling under the exclusive lock, because the step that
compiles says `lock = "shared"` right there in the file, where it can be reviewed, instead of
being invisible inside a script.

### A recipe declares a procedure. A run records an event.

The recipe must **not** name revisions. Pinning them there binds the procedure to a moment and
makes it progressively harder to rerun, which is the opposite of what putting it in the repo
was for. But dropping revisions entirely would lose the provenance that makes a number worth
keeping. The two wants are only in conflict if the same file tries to serve both.

They do not have to:

- **The recipe** says which repos it needs and how to measure. No commits.
- **The invocation** supplies the code: `dibs bench cubek@main gemm-roofline`.
- **The run record** captures the resolved reality: the commit of every repo in the workspace,
  the device, the isolation, the recipe hash, the driver version, the time.

That is the same split as a Makefile against a build log, or a test against a test result. The
recipe stays runnable against code written next year; every historical number stays fully
identified. Nothing is pinned and nothing is lost.

**dibs records the resolution, it does not perform it.** These repos develop against each other
through local path dependencies, so which cubecl a cubek build sees is a property of the
working tree, not a declaration anyone made. Trying to control that from here would be writing
a package manager next to cargo. Reading `git rev-parse HEAD` in every repo in the workspace at
launch, and filing it with the run, is complete and costs nothing.

The one pin that is legitimate in a recipe is a **named baseline to compare against**, because
that is a parameter of the comparison rather than of the procedure. The code under test is
supplied; the thing it is measured against may be fixed.

**What this does not rescue, said plainly:** rerunning a benchmark from three months ago means
checking out that ref and running the recipe it had, and that may simply not build any more.
The design makes the run identifiable and the procedure recoverable. It does not make old code
compile, and no amount of specification will.

### The recipe's content has to be recorded with the run

A label alone is not provenance. `gemm-roofline` at two different refs can be two different
benchmarks filed under one name, and comparing across that is the failure the whole history
exists to prevent. So the run records the ref and a hash of the recipe it used, and a
comparison against runs made under a different recipe hash says so rather than silently
averaging two things.

### What still does not justify a per-call graph



With steps in the recipe, what remains of the per-call graph idea is queueing several
*different* recipes at once with dependencies between them. That is still not worth building.

**The reservation is a lock property, not a request format.** Splitting build from measure honestly costs the agent its place in the queue: it
finishes a two minute build under the shared lock, asks for the exclusive lock, and goes behind
whatever arrived meanwhile. The rule asks agents to do something worse for them, which is a
good explanation for why the split does not happen on its own.

The gate already implements the fix. A job holds the gate while it releases its shared lock and
takes the exclusive one, so nothing can get in front of it during the transition, and it never
blocks: the other shared holders are running and do not need the gate to finish. This must be
release-and-requeue under the gate, **never a lock upgrade**: two jobs each holding the lock
shared and each waiting to upgrade deadlock with no way out.

**A graph needs state that outlives a step, which is the one thing the design refuses.** The
lock is an open descriptor held by a live process, so a machine that loses power comes back
unlocked, and there is nothing to clean up. A half-executed graph holding a reservation for a
step that has not started is precisely the orphan this avoids, and it would need an owner, a
timeout, and a reaper: a scheduler, in other words, arriving through the side door.

**Batching to save agent turns is worth less than it looks**, because a call launched in the
background already reports back on its own. The saving is one round trip, against a format to
learn, write and debug. *Wrong, and measured wrong: a completion is not a round trip, it wakes
the agent for a full context re-read, so one job is three turns and a hundred jobs are three
hundred. See `batch.md`.*

### What the log can and cannot say about this

36 of 38 exclusive jobs run a script file or a shipped blob, so the log cannot see what happens
inside them and cannot measure how much preparation is being done while holding the machine.
Of the two it can see, three are `cargo bench --no-run`, which is a compile under the exclusive
lock, and those are the runs already measured at 31.8 minutes, 17% of all exclusive time ever
recorded. The need is real. Its size is not knowable from here, which is its own argument for
the verbs: they make it visible.

### When a spec does become right

The four-GPU machine. "Build once, then measure the same suite on each of four devices" is a
genuine graph: one shared build and four exclusive device acquisitions that must not serialize
behind each other. No single command can own that, because the four measurements are separate
lock holders with separate lifetimes.

Even then the shape is a **bounded matrix**, one recipe expanded over a set of devices, not a
general dependency graph:

```
dibs bench cubek@ref gemm-roofline --on-each gpu:nvidia
```

A general DAG is a build system, and cargo is already the build system here. The moment the
format can express something cargo cannot, it has stopped being a way to ask for hardware.

## Migration

There is no flag day. `raw` is how everything works today, so everything keeps working, and
recipes are added as patterns show up in the gaps report. The verbs arrive one at a time, each
one useful alone, and `bench` is worth doing first because it is where a wrong command costs a
wrong number rather than a retry.

## Artifacts: a job declares what it produces, and later jobs consume it

Not built. The idea is that a step names its outputs, dibs collects them, and a later step on
another machine can take them as inputs. The motivating case is a benchmark machine that is
busy: build the binary elsewhere meanwhile, ship it in, and measure the moment the lock frees.
The orchestrator is the natural bridge, since it is the only host that talks to all of them.

**The two halves have completely different risk, and the split is the design.**

**Results are portable, and this half is worth doing on its own.** A benchmark's numbers
currently end in `$SCRATCH/out/<label>.log` on the machine that produced them, while the run
record carries the commit, the isolation and the duration but not the measurement. Collecting a
declared result file to the orchestrator is what turns `runs` from what ran into what was
measured, which is the thing all of this exists for and the one step it stops short of. Small,
always safe to move, and no compatibility question arises.

**Binaries are portable only within an ABI class, and that decides whether the rest is worth
building.** A binary runs on another machine only if the CPU feature baseline, glibc, the GPU
driver and rustc all line up. `-C target-cpu=native`, which benchmark profiles reach for
routinely, bakes in the building machine's instruction set and will fault on a different one.

Two things narrow it usefully. **Cross-machine reuse is only useful for GPU benchmarks, and
that is also where it is safe**: cubecl compiles kernels at run time, so the GPU half is not in
the binary at all, and for a GPU benchmark the host code is not what is being measured. For a
CPU benchmark the host binary *is* the measurement and a foreign one is worthless by
definition, so the risky case and the useful case do not overlap.

And **the shape already exists in this design**. Devices carry a compatibility key
(`fingerprint`, will these kernels run here) separate from a binding key (the PCI id, is this
the same physical card). An artifact needs exactly that pair: a content hash for identity, and
a build-compat key for whether it may be reused here, computed from the CPU baseline, glibc,
driver and toolchain versions.

**The compat key came first, before any artifact movement**, to answer whether reuse is
available in this pool at all for the price of a probe. `dibs --abi --all` reports the facts
that decide it, per machine, and compares each pair in each direction, because the answer is a
relation between machines rather than a property of one. Architecture, x86-64 microarchitecture
level, glibc, rustc and the GPU driver.

**Measured 2026-08-31, and the answer was yes**, which was not the expected one: a laptop with
an Intel iGPU and a desktop with an AMD CPU and an NVIDIA card came back identical on every
axis, both x86-64-v3 on glibc 2.43 and rustc 1.98.0, because they run the same distribution at
the same version. Homogeneity of the operating system did more for this than any property of
the hardware, which is worth knowing: the compatibility being asked about is mostly a fact
about how the machines are administered.

One caveat the probe states itself and cannot check: equal levels are not equal instruction
sets. `-C target-cpu=native` reaches past the level it lands in, so a binary built that way can
still fault on a machine the probe says yes about. What it covers is reuse without it.

So artifact movement is not ruled out here, and the remaining work is what it always was:
naming, tracking, a garbage collection rule of the kind worktrees already have, and deciding
whether the reuse is worth the machinery. The transport exists, since `--sync` moves files and
takes the shared lock like any other work.

## Open

- **How much a recipe parameterizes.** A recipe that takes no arguments is rigid; one that
  takes arbitrary arguments is `raw` with extra steps. The likely answer is named parameters
  declared in the recipe, so the set of valid invocations stays enumerable.
- **Where an exploratory sweep sits.** It is a real benchmark that wants the exclusive lock and
  a stable label, but it is genuinely one-off. Possibly `dibs bench --script <path-in-repo>`
  with an explicit label, so it gets the locking and the recording without needing a recipe.
- **Whether agents should be able to write recipes.** They are the ones who know the command.
  Letting them propose a recipe as a pull request against the repo is the obvious answer and
  makes the gaps report a queue of pull requests rather than a queue of work for one person.
