# Settled, deferred, open

Design records for the lock itself. Each entry is here because it was expensive to work out
and cheap to get wrong again, and several of them read as arbitrary until you know the
measurement behind them.

## Settled. Do not re-open

**The idle signal is a rate, not a total.** Each look leaves what the holder's process tree had
burned in `cpu.<pid>`; the next flags it if that has not moved. One sample says nothing. A
cumulative count only ever caught a job wedged before it started, never the sweep that hangs
after an hour, which is the case that matters.

**Estimates fall back label, then agent, then mode**, and the history records which agent ran a
job. Two thirds of labels ever recorded appear exactly once, because agents name the *run*
rather than the *kind of work*, which is the whole reason estimates looked broken. Measured on
229 real rows: agent scope is off by 1.8x at the median against mode scope's 2.5x, so the
middle rung earns its place.

**An estimate carries p10 and p90, not just the median.** Half of all labels name a repo rather
than a kind of work, so one label can cover a `git status` and a full build across 29 runs, and
its median is honestly zero. Wide spread is reported as a range rather than a number, a job past
its median is bounded by p90 rather than abandoned, and "stuck" is measured against p90.
Windowing to recent runs was tried first and rejected on measurement: 1.1x median error either
way, because the spread is inside the labels rather than in their age.

**Queued shared jobs do not wait for each other.** The shared lock admits all of them at once,
so a queue only advances at a benchmark, and that benchmark waits for the longest shared run
rather than their sum. Verified directly: three queued shared jobs start within 5ms of each
other. Summing them was the bug this replaced.

**A quick shared job may skip a queued benchmark**, when the benchmark has waited less than
`DIBS_PATIENCE` (60s) and the job's own median is under `DIBS_QUICK` (10s). Its own median, not
its mode's. Worst-case delay is their sum. This is safe to tune because the gate is only about
fairness: once a benchmark holds the lock, flock refuses everyone regardless, so a bug here can
delay a benchmark but never contaminate one.

**`--out` reads a job's output by asking the kernel, not by capturing it.** A job's stdout is
the ssh channel and nothing keeps a copy, but jobs redirect into files, and the process walk
that already exists for CPU accounting resolves fd 1 and 2 across the tree to find them. It has
to skip anything equal to the root's own stdout, which every descendant inherits, or it hands
back the caller's terminal instead of the job's output.

**Transfers go through `--sync`**, which is rsync pointed back at the wrapper through its own
`-e`, so the far side holds the shared lock while rsync's protocol stream passes through as the
workload. A copy is not free: it competes for memory bandwidth and writeback with whatever is
being measured.

**A full Rust rewrite of the remote half was considered and rejected.** Shipping the remote
script per call is what keeps the target with nothing installed on it, which is the design's
best property. The interface layer is Rust; the wrapper stays bash.

## Deferred. Do not re-derive

**Round-robin or fair-share across agents.** Nothing is starved today, only delayed. The good
version picks the next job by which agent has used the machine least, but that means replacing
kernel arbitration with a userspace ticket queue, and losing "the lock dies with the process".
Observed benchmark counts per agent were 7, 3 and 1: nobody is flooding.

**Checkpoints so a long benchmark yields mid-run.** Reacquisition makes sweeps take hours, and
interleaving damages comparability *within* a sweep specifically, because each measurement then
inherits different cache and thermal state. Better shapes, in order: split a sweep into
per-point invocations, which needs no new machinery and fixes labels too; escalate to exclusive
only around measured regions; preempt long *builds* for benchmarks rather than the reverse,
since a compile is indifferent to SIGSTOP and a benchmark is not. Pinning GPU and CPU clocks is
the prerequisite that would make any of it safe, and is worth doing on its own.

**A JSON step-spec submitted per call.** Superseded in part, see `batch.md`. The reasoning
below held that a spec adds nothing over commands except a reservation, and it is still right
about the machine. It was wrong about the caller: it costed a background completion as one
round trip, when a completion wakes an agent for a full context re-read, so one job is three
turns. A session that launched about a hundred jobs one at a time spent most of an 870M-token
day doing it. What survives unchanged is the rest of the entry.

  It compiles down to two invocations that already work. The only thing a per-call spec adds
  over commands is a *reservation*, so the exclusive phase does not go to the back of the queue
  after the shared build. If that is ever built it is a flag, not a file format, and the
  transition must be release-and-requeue-with-priority, never a lock upgrade.

## A shared sccache is the next real lever, and it is cheaper than artifacts

Measured on the benchmarking machine, 2026-08-31: two build caches totalling 13.8G serve seven
worktrees that cost 72M between them. One target directory per repo rather than per worktree is
already doing the work, and giving each tree its own would multiply the 13.8G by the number of
trees. Disk is not the problem and was never going to be.

What is unsolved is that a cache is a fact about one machine. Cache affinity has to be a strong
rule precisely because a cold build costs minutes, and that is what confines a second machine to
repos nobody benchmarks.

`sccache` addresses that directly. It caches `rustc` invocations keyed by source, flags and
compiler version, so it is shared across worktrees, across repos that share a dependency tree,
and across machines when it is pointed at shared storage rather than a local directory. The
precondition is a matching toolchain, which `dibs --abi --all` already reports and which held
here: rustc 1.98.0 on both machines.

The consequence is the interesting part. **It weakens cache affinity rather than replacing it**,
and a cold machine that is no longer really cold is what makes load ranking worth having. That
is a larger effect than moving artifacts, and a much smaller thing to build.

Limits, stated so nobody rediscovers them: it caches neither linking nor `build.rs` execution,
so a cold machine still pays both. It needs `CARGO_INCREMENTAL=0`, which costs nothing for
release builds and something real for the test recipes. And the linked binary still exists only
where it was built, so this makes building elsewhere cheap rather than making the result usable
elsewhere.

It also fits the rule that nothing here may have a component whose death wedges every machine.
A shared sccache that goes away produces cache misses, not a stuck pool.

**Where the storage lives is decided by contamination, not by convenience.** Not the
orchestrator, which sleeps and moves, for the same reasons it is marked `measure = false`. And
not the benchmarking machine either, though it has the disk and the uptime: serving cache reads
during a measurement is CPU, IO and network on the machine being measured, which is the thing
`--peek` is restricted for. A separate always-on host is the right shape, and it can be modest,
since it stores objects rather than compiling anything.

Two sccache features are easy to confuse and only one is wanted. Shared *caching* needs shared
storage, Redis or S3 or WebDAV, and the compile still happens on the requesting machine.
`sccache-dist` farms the compilation itself to build servers and brings a scheduler and
toolchain distribution with it; an old machine makes a fine object store and a poor build
server.

The condition that decides whether a cache host is worth having: **a hit must be faster than
recompiling.** Wired rather than wireless, and no sleep. Many small objects over a slow link
can lose to running rustc, which would mean adding a machine to make builds slower. The default
10G cache cap also needs raising, since one repo's target directory here is 12G.

**Co-hosting it with the agents is fine, and there is nothing else to co-host.** The dispatcher
is the client, per call, with no daemon, so "the orchestrator machine" is only wherever `dibs`
is run from. An always-on box that runs the agent sessions and stores the cache is one machine
doing two undemanding things, and it is strictly better than running agents from a workstation
that sleeps.

It also corrected a rule. Routing used to discount whichever machine was dispatching, on the
reasoning that it is the one someone is sitting at. A headless box running the agents breaks
that: nobody is sitting at it, and discounting it would waste the build node with the fastest
possible access to the cache. Being a workstation is a property of the machine, so it is an
inventory field, and `--check --write` offers it where it finds a battery while saying to drop
it when the machine is headless.

**Measured 2026-08-31, and it is decisive.** `cargo build --release -p benchmarks --features
cubecl/cuda`, built twice into a fresh target directory on the benchmarking machine:

| | wall | hit rate |
|---|---|---|
| cold target, cold sccache | 183s | 0 of 275 |
| cold target, warm sccache | 16s | 275 of 275, 100% |

Eleven times faster, and the entire dependency tree occupies **296 MiB** of cache. Two facts
follow that change the shape of the plan.

**A machine holding no cache for a repo is cheap, not expensive.** The whole argument for cache
affinity being an absolute rule was that a cold build costs minutes. It costs sixteen seconds.
Once a shared sccache is reachable from both machines, affinity should fall back to a weighted
preference, because queueing behind a twenty-minute benchmark to save under three minutes is the
worse trade. Until then the rule stands as written: nothing is shared yet, and a cold build
really does cost 183s today.

**Measured on a much larger workspace too, and the ratio holds.** Its seven engine crates, the burn and
cubecl-heavy half:

| | wall | hit rate | target dir | cache |
|---|---|---|---|---|
| cold target, cold sccache | 188s | 0 of 583 | 1.9G | 462M |
| cold target, warm sccache | 19s | 583 of 583, 100% | 1.9G | 462M |

Ten times, against cubek's eleven, so the effect is not an artifact of a small repo. The hits
include 65 C/C++ and 14 assembler compilations, which matters for a workspace with real native
dependencies: sccache is not only caching rustc.

And the whole workspace, once the ALSA headers the README asks for were installed:

| | wall | hit rate | target dir | cache |
|---|---|---|---|---|
| cold target, cold sccache | 922s | 0 of 1536 | 6.5G | 1.1G |
| cold target, warm sccache | 104s | 1517 of 1536, 98.8% | 6.5G | 1.1G |

The ratio decays gently with size, 11x then 10x then 8.9x, in the expected direction: the 104s
floor is linking and `build.rs`, which sccache never caches, and they are a larger share of a
larger workspace.

**A cache host needs almost nothing.** 1.1G for that whole workspace, 462M for its engine crates,
296M for cubek: between a sixth and a quarter of the target directory each time. Every repo here
together comes to single-digit gigabytes. The disk was never the constraint, which means the
host can be any machine that is always on and is not being measured. Latency on a hit is what
matters, so the link decides it and the storage does not.

**The number that was believed to be hundreds of gigabytes is 6.5G.** One full release build of
the largest repo, one configuration, one commit. So the disk pressure that is really observed is
accumulation over time rather than the cost of any one build, and the fix for it is collection
rather than compression.

**Surveying a working machine says where it actually accumulates**, and it is not one place.
On a developer laptop at 220G used: 40G of target directories inside repos, 65G of downloaded
model checkpoints, 20G of cargo registry and git checkouts, 19G of VM images, and **16G of cargo
target directories inside `~/.cache`**, under a name given by whoever set `CARGO_TARGET_DIR`
there for a test sweep across three backends.

That last one is the finding worth keeping. `cargo clean` cleans the workspace it is run in, so
a target directory placed anywhere else is invisible to the habit everyone already has, and it
accumulates until someone goes looking. It is the same failure the old log showed for worktrees:
107 of 179 jobs wrote a hand-chosen path, and nobody could collect what nobody could enumerate.
**Owning the layout is what makes collection possible**, which dibs already does on the
benchmarking machine, where the same two repos cost a tidy 13.8G.

So the gap was narrower than "disk is a problem" and sharper: worktrees were collected after
`DIBS_KEEP_DAYS` and no target directory was collected at all, including the ones dibs itself
creates. Both are enumerable because dibs put them there, which is the whole reason either can
be collected.

**Closed, on a separate and much longer clock.** A worktree is per commit and disposable; a
target directory is per repo, shared by every tree of it, and is the thing that makes a build
fast rather than a by-product of one. So it is removed only when a repo has stopped being built
on that machine at all, at `DIBS_TARGET_KEEP_DAYS`, defaulting to 45 days against a worktree's
14. The compilation cache is what makes even that safe: a collected directory costs 104s to
refill rather than 922s.

A directory with no marker is given one rather than removed, because everything already on a
machine predates the marker and reading that as "never used" would delete every target directory
on the first run after an upgrade.

**And sccache does not shrink target directories.** It makes filling one fast. 100G stays 100G
on every machine that builds it; what changes is whether producing it costs minutes or an
afternoon. Disk pressure on the build machines is a separate problem with separate answers: one
target directory per repo rather than per worktree, which is already done, and collection, which
exists for worktrees and does not exist for target directories at all.

The 16s floor is itself informative: it is linking and `build.rs`, neither of which sccache
caches, so it is what every machine pays no matter how warm the cache is.

## Open

**The most common gap is not a missing recipe, it is a missing parameter.** `dibs-run gaps`
names "verify a cubecl PR against the cubek tile engine" six times, more than everything else
put together, and cubecl's own pull request template mandates exactly that: build cubek and
burn against the PR's hash before it can merge. It cannot be written as a recipe, because the
thing that changes every time is which cubecl revision to test against, and a recipe has no
parameters. Nor can the caller pass one: recipe steps are strings run on the machine, ssh
forwards no environment, and nothing substitutes into them.

So the recurring workflow that the upstream process requires is the one thing the interface
cannot express, and it has been done through `shell --reason` six times instead. That is the
specification for parameters, and it is worth more than any individual recipe. It would also
fix `shell` collapsing into one label: `cubek/shell` has run eleven different procedures under
that name, so its history means nothing, which is the same defect as two devices under one
label one level up.

**One assertion in the suite is load-sensitive and has not been identified.** Under six
spinners the suite went 219, 219, 218. The one that used to fail there was the reaped-children
CPU count, which measured wall time and now burns CPU time instead; something else is still
sensitive and calling the suite flaky is not a diagnosis.

**Environment knobs do not reach the machine.** ssh does not forward the environment, so
`DIBS_IDLE_AFTER` and its neighbours only take effect in local mode, which is how the test
suite runs. Two bugs have already come from code placed on the wrong side of that boundary.

**`--max` defaults to 30 minutes for shared jobs.** A genuinely long compile would be killed
with exit 124, and splitting builds out of benchmarks makes long shared jobs more likely.

**The live suite's rsync round-trip case has never run**, because the machine has not been idle
long enough. The round trip itself is verified by hand; the harness case is not.

**Jobs that redirect nowhere still cannot be read.** Capturing them needs a `tee`, and it must
be process substitution per stream: piping `2>&1` into one `tee` would merge the streams the
caller sees and change what every job hands back. Not worth that risk for the minority case.

**Nobody has checked whether agents can actually use this interface.** Every verb, flag and
rule here was decided from the log of what agents did with the *old* interface, which is
evidence about the problem and not about the fix. Whether the current one is understood, or
merely obeyed while being misread, is unmeasured. The way to find out is the way the first
round was found: read the transcripts of agents that have used `dibs`, and look for the
confusion rather than the failures. A wrong call that still works teaches nobody anything and
shows up nowhere in the log, so the transcript is the only place it is visible. Worth doing
once there are enough sessions to be worth reading.

**Concurrent builds of one repo serialise on cargo's own lock**, since they share its target
directory. Two agents on different worktrees of the same repo wait for each other. sccache is
the answer to this one as well, and it is the same piece of work.

**A long-running reader is invisible to every diagnostic here.** `--watch` takes no lock, so it
appears in neither `--status` nor `--log`. Four orphaned feeds polled the machine every two
seconds for four hours before a person noticed them by eye. Whatever else is true of the lock,
the thing that costs a shared machine is not always holding it.

**What `--status` reports as a job's CPU is its process tree, and sccache empties that tree.**
The compiler runs inside a daemon parented to init, so a build using every core reads as near
zero. Idleness no longer depends on it, since a job whose log is growing is working, but the
column still shows a number that is true and misleading. Either it says nothing when a job is
alive but tree-idle, or the daemon's time is attributed to whoever holds the lock, which is
right until two shared jobs compile at once.

**Whether the largest workspace gets a recipe, and at what scope.** It builds in full only where
ALSA development headers are installed, which is a real dependency of the chat app and nothing
to do with measuring. The alternative is a recipe scoped to the engine crates, which is what
would actually be measured. A recipe that quietly builds part of a workspace is the kind of
thing that misleads six months later, so whichever it is has to say so.

**Rules written for agents tend to be over-broad.** The scratch rule had to be narrowed after it
read as forbidding an agent from syncing source into a worktree. Expect others.

## Two flaky tests, and the shape they share

The queue-ETA test compared a rendered duration derived from the clock, so a second ticking over
mid-test failed it. The idle test asserted on a single sample of a signal that is deliberately a
rate. Both now test the design instead: relations between ETAs, and a primed first sample. An
assertion on rendered output is an assertion on the clock.
