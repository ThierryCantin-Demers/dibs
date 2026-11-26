# Routing: each job on the machine its kind belongs on

Status: proposed.

## The problem

A team shares compile machines, GPU machines and benchmark machines. Today a call runs every job
on one machine. The usual workflow sends a tree, builds it under the shared lock and measures
it under the exclusive lock, so the benchmark machine spends most of its time compiling: a large
build there takes four to six minutes before a measurement of a few minutes, and the
measurement waits behind every other build queued on it. Shared work with no card in it, clippy
and unit tests, which is most rows of most batches, runs wherever the caller named.

What is wanted:

- Shared work goes to a compile machine. Shared work that asks for cards goes to a GPU machine
  that has all of them. Exclusive work goes to a benchmark machine.
- A recipe's build runs on a compile machine and its measurement on the benchmark machine, with
  only the executables and what they need carried across. Target directories reach hundreds of
  gigabytes, so they never move.
- It works with recipes as they are written, and with everything dibs does: values, A/B over
  refs, `--reps`, `--sweep`, `--pin`, `fresh`, `artifacts`, images, `with`, `shell`, `run`,
  batches, check-ins, label series, `built=`, `tests=` and exit 78.
- Direct calls and the dibspatcher place work by the same rules, from the same code.

## Roles

Each machine in the inventory takes one or more roles: `compile`, `gpu`, `bench`. A machine that
measures and also runs GPU tests has both of the last two. The inventory becomes a team
checkout that `dibs --update` refreshes, as recipes already are, so every caller and the
dibspatcher read the same machines and roles. A personal `machines.toml` still adds machines of
one's own, which keep working as today.

A job that names no machine is placed by its kind:

| Job | Role | Chosen by |
| --- | --- | --- |
| shared, no card | `compile` | the repo's cache affinity, then load |
| shared, `--device` | `gpu` | every requested card on one machine, then load |
| exclusive | `bench` | the label's series (machine and card), else a free card of the model |

`--on` still names a machine, and overrides the role.

**A label's series is read from the machines, not the caller.** Each benchmark machine's runner
can record the labels measured on it and their cards (`dibs-runner/src/series.rs`), which stage 1
turns on everywhere. Series callers already hold are not lost: a call whose caller has a series
the machine lacks sends it, and the runner records it once. A replacement machine takes its
predecessor's series through the inventory's `series` key, as now. Placement
asks every `bench` machine that could take the call: one with a series of the label takes it;
several means the most recently measured, and the call says which and where the others are; none
means a new series on a free card of the model. `--new-series` ignores the old binding and takes
a free card of the model, and the runner records the new series once a measurement succeeds, as
it does now. All of
this happens before any arm is built, so a direct call and a dispatched one resume the same
series.

## A call becomes placed jobs

The planner (`execution/schedule.rs`) already orders a call's jobs: every arm's send, setup and
build, then the measurements alternating ABBA. Each job gains a destination, chosen once when
the call is planned and recorded:

1. Each arm's source is sealed once: a ref resolves to its commit, and an `@local` tree or pin
   to a snapshot with a digest. Every machine that receives the arm gets that snapshot and
   checks its digest, so a branch that moves or a file edited meanwhile cannot put a new tree
   beside an old binary.
2. Each arm is built on a compile machine. Arms may build on different compile machines at once
   only when their build signatures match: the same toolchain, image digest and flags, with no
   `target-cpu=native`. Otherwise they build one after the other on one machine, so a
   comparison never measures a difference between compilers.
3. The build job seals its bundle itself, before it ends, by copying what the bundle holds into
   an immutable store on the compile machine. For the whole job, from its first cargo call to the
   seal, it holds a lock on its target that every job writing into that target takes, so two
   refs sharing a target take turns and no build can replace an executable between cargo
   reporting it and the seal reading it. Cargo already serialises builds of one target, so this
   costs nothing new. A `--low-priority` build that parks lets go of the lock with its build
   mark, as it lets go of the target today, and its capture starts again with the build when it
   runs again.
4. The bundle is carried to the benchmark machine and staged there.
5. The measurements run on the one benchmark machine and card set, in today's order. `--reps`
   reuses the staged bundles, and a sweep stays a batch of ordinary calls, whose identical
   builds share one bundle by digest.

**Which shared steps are builds** uses the classification dibs already makes to mark a step
preemptible (`compiles_only`, `execution/jobs.rs`): in a call that measures, a shared step that
only compiles goes to the compile machine, and any other shared step after the last build, such
as a warmup script, runs on the benchmark machine in recipe order, beside the measurements. A
shared step that is not a build and comes before one may write something the build reads, a
file an `include_str!` takes, so its call stays whole. A call that measures
nothing is placed whole by the table above: on a GPU machine with its cards if it names any,
`cargo test --device` among them, else on a compile machine.

**`fresh` values belong to the plan.** Each arm's token is chosen once, stored with the placed
plan, and every rep's value is derived from it as today (`execution/base.rs`), whichever machine
runs the job and however often a worker retries it.

## Deciding to split, before anything runs

The decision is made when the call is planned, for every arm and rep at once, never discovered
halfway through an A/B. A call splits only when all of these hold:

- every shared step it sends to the compile machine is a recognised compile-only cargo command,
  and no other shared step comes before the last of them;
- every measured step runs a cargo invocation dibs recognises in its text after values are
  bound (`cargo bench`, `cargo test`, `cargo run`, with their arguments) or an executable by its
  path under `$CARGO_TARGET_DIR`, and reads nothing else of the target;
- no step outside the builds writes into the target;
- the flags compile for a CPU baseline, not `native`;
- no step runs doctests.

A measured step that runs a script is opaque: dibs cannot know what it calls, so the call stays
whole. Everything else that fails the test also runs whole on the benchmark machine, as every
call does today, and the summary says which step kept it there and why. A recipe can say
`split = false` to skip the test. Nothing is refused for not splitting.

**Every arm's plan is checked before the first measurement.** The text test cannot see that one
arm's `cargo run -p x` names a binary only the other arm's build makes. So each arm's build also
runs, for every measured cargo invocation, its counterpart that runs nothing (`cargo bench
--no-run` with the same arguments, `cargo build` with the same selection for `cargo run`), which
reports exactly the executables that invocation would launch, and compiles them on the compile
machine if the build step had not. Once every arm is built and staged, every measured invocation
of every arm must resolve in its launch plans. If any does not, the call falls back as one before
anything is measured: every arm builds again on the benchmark machine, as today.

At run time the benchmark side still refuses anything the plan did not foresee (below), as a
refusal of the split, kept distinct from a command of the recipe that exits 78 itself
(`Stepped.refused`, `execution/base.rs`).

## Capturing what was built

A job-local `cargo` wrapper on `PATH` adds `--message-format=json-render-diagnostics` to the
subcommands that build (`build`, `bench`, `test`, `run`, `rustc`), before any `--`. Right after
each invocation returns, and before the next can overwrite a path, the wrapper copies the
executables it reported into the store by digest, so each launch plan names the digest its own
invocation produced. Two invocations that leave different executables at one top-level path,
`release/examples/x` built with two feature sets, cannot both sit in the sparse target: a call
that would launch both by that path stays whole. Every other
subcommand, `metadata` and `check` among them, passes through untouched. The JSON goes to a
report of the job's own; the caller sees the diagnostics, the exit and any JSON it asked for,
unchanged. Each report records the executables cargo built or found fresh, indexed by package,
target, kind, profile and features, and the `OUT_DIR` of every build script.

The bundle holds:

- every executable a report names, at its path under the target, both cargo's hashed path in
  `deps/` and the top-level alias, so a step calling `$CARGO_TARGET_DIR/release/x` finds it;
- the shared libraries those executables load from the target, followed through their ELF
  dependencies;
- the whole build-script directory (`build/<package>-<hash>/`: its `out`, `output` and the rest)
  of each of the workspace's own packages, up to a size cap, since a binary may read a generated
  file at run time. A call whose directories pass the cap stays whole. Anything else under the
  target a measured binary reads is invisible to dibs: a recipe names it with
  `carry = ["<glob under the target>"]`, or says `split = false`;
- a launch plan per invocation: what `cargo bench`, `cargo test` or `cargo run` with those
  arguments would have run, in order, with its working directory, its `CARGO_*` environment and
  its library path. For `cargo bench` and `cargo test` that is the harness executables, the
  artifacts cargo reports as built for tests (`profile.test`), selected by the invocation's
  filters; the package's binaries built beside them for `CARGO_BIN_EXE_*` are carried as files
  and never launched. For `cargo run` it is the one binary or example it selects;
- a manifest binding each file's digest, path and mode to the sealed source, pins, lockfile,
  toolchain, flags and image.

The sealed source goes to the benchmark machine as trees reach machines today, kept per tree and
sent as a delta, so scripts, configs and data in the tree are there without being declared.

## Running it on the benchmark machine

The measured step runs its own command, unchanged. Its `PATH` puts a `cargo` first that answers
from the launch plans: `cargo bench --bench x -- args` runs the staged `x --bench args` from its
package root with cargo's environment, as cargo would. `$CARGO_TARGET_DIR` points at the sparse
target, so `$CARGO_TARGET_DIR/release/x` is the staged `x`. An invocation that matches no plan, or
that would compile anything, is refused before running, naming the invocation. Nothing is ever
compiled under the exclusive lock.

Paths compiled into a binary, `env!("CARGO_MANIFEST_DIR")` and `OUT_DIR` among them, name the
compile machine's tree, so the tree and the sparse target sit at the same absolute path on both
machines (see the open question below).

Staging runs as a shared job on the benchmark machine before the first measurement: it checks
every digest, that every library resolves, and that glibc, the loader and the card's driver
accept the binaries, as the pilot's stage does. It runs natively, never in the build's image,
since the measurement runs natively too and a library present only in the image would pass
there and fail to load here. The measurement checks the staged digests again
under its lock and exits 78 if anything moved.

## Moving bundles between machines

The compile machine's runner seals the bundle; then whoever drives the call, a laptop or the
dibspatcher, asks it to push to the benchmark machine. The bytes never pass through the driver.

- Each compile machine has its own key on each benchmark machine's runner account, with a forced
  command that only receives into that machine's staging, for a transfer the driver announced.
- The receive is a shared job on the benchmark machine, admitted before any byte moves, so it
  never runs beside a measurement. The compile side holds its shared lock while it sends.
- Files are stored by digest, so a bundle another arm, rep or sweep point already sent costs
  nothing. A partial transfer resumes. Bundles are leased while a call refers to them and
  collected by age and quota, under `dibs --gc`.

## Records

A run record keeps its measured machine and card. Each step record gains its machine, card,
attempt and the bundle it used. `built=` is the compile job's, and the measurement reports none.
`tests=` is the job that ran the tests. Artifacts come back from the job that wrote them. `dibs
out` and `--fetch` find each job on its machine.

## The dibspatcher

The dibspatcher runs the same planner on ordinary calls and batches, not hand-written splits.

- **What a submission stores:** what a detached batch carries today, the call or batch text,
  the sealed trees and pins, the caller's recipe overrides, label and identity, and the caller's
  execution settings: check-in interval, `--low-priority`, preemptibility and image. A step
  resumed or added later takes them from the submission.
- **Every job gets an attempt id,** not only the last one. What a runner keeps for an attempt
  grows from its trailer to the job's whole result, the `Prepared` of a setup included, so a
  worker that resumes after any job can carry on from what that job produced, instead of
  starting it again.
- **Check-ins:** the worker follows a job that checks in, as a detached driver does, and its
  submission keeps going. A caller's `dibs dispatch follow` checks in on its own clock, as now.
- **Kill** stops new launches, then every open job on every machine.
- **`--artifacts <dir>`** names a directory on the caller's computer. The dibspatcher keeps each
  job's artifacts and records where they came from; the client that follows the submission, or
  a later `dibs dispatch fetch <id> <dir>`, puts them in the directory at today's sweep point,
  arm and rep paths.
- **What stays on the direct path:** a `with` whose command runs on the caller's side, and
  `run --hold`, need the caller's computer, so the dibspatcher refuses them, as a detached batch
  does. `with --there` is dispatched.
- A detached batch whose steps span machines is driven by the dibspatcher; one on a single
  machine is still driven there.

## Stages

Each ships and is tested on its own:

1. **Roles.** The team inventory, roles, series read from the machines, and placement of whole
   calls by their most demanding job. Shared work leaves the benchmark machines at once. Rows of
   a batch are placed one by one only when each is a recipe, which builds what it measures
   itself. Rows linked by `after=` where any is a `run`, a `shell` or a `--sync` may share what
   an earlier row wrote, a binary under `$CARGO_TARGET_DIR` included, and are placed together on
   the machine their most demanding row needs, so a build row and the benchmark after it stay on
   one machine. A detached batch stays on one machine, the one its most demanding row needs,
   until stage 2 gives it a driver that spans machines.
2. **Ordinary work through the dibspatcher.** Calls and batches as above, with per-job attempts
   whose results can be replayed, still one machine per call.
3. **Splitting.** Sealed sources, the split test, capture, bundles of several executables,
   direct transfer, staging, the cargo on the benchmark side, A/B across machines, and the
   fallback.
4. **`with` servers** split the same way, their binaries carried like any other, for `--there`
   and for the default form whose command runs on the caller's side. There the attached caller
   drives the transfer, and its command and port forwarding stay as they are.

## Open questions

- **The common path.** Either every machine in the pools keeps its trees under one absolute
  path, set when it is provisioned, or the measured step binds the compile machine's path over
  its own in a mount namespace, as builds in an image already do. The first adds nothing to the
  measured process; the second needs nothing from provisioning.
