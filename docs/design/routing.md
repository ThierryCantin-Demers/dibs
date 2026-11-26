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

`--on` still names a machine, and overrides the role. A label already measured stays on its
machine and card, as now.

## A call becomes placed jobs

The planner (`execution/schedule.rs`) already orders a call's jobs: every arm's send, setup and
build, then the measurements alternating ABBA. Each job gains a destination, chosen once when
the call is planned and recorded:

1. Each arm is sent and built on a compile machine. Arms can build on different compile
   machines at once, so an A/B builds its two arms in parallel.
2. Each arm's build is sealed into a bundle on its compile machine, while its target still
   belongs to it.
3. The bundle is carried to the benchmark machine and staged there.
4. The measurements run on the one benchmark machine and card set, in today's order. `--reps`
   reuses the staged bundles, and a sweep stays a batch of ordinary calls, whose identical
   builds share one bundle by digest.

A shared step after the build that asks for cards, such as a warmup on the card, runs on the
benchmark machine before the measurements, in recipe order. A call with no exclusive step and
no card is placed whole on a compile machine.

## Capturing what was built

The build's cargo invocations report what they produced. A job-local `cargo` wrapper on `PATH`
adds `--message-format=json-render-diagnostics` before any `--`, writes the JSON to a report of
its own, and gives the caller the diagnostics, exit and any JSON it asked for, unchanged. It
covers cargo called from scripts as well. Each report records, per invocation, the executables
cargo built or found fresh, indexed by package, target, kind, profile and features.

The bundle holds:

- every executable a report names, at its path under the target, both cargo's hashed path in
  `deps/` and the top-level alias, so a script calling `target/release/x` finds it;
- the shared libraries those executables load from the target, followed through their ELF
  dependencies;
- a launch plan per invocation: what `cargo bench`, `cargo test` or `cargo run` with those
  arguments would have run, in order, with its working directory, its `CARGO_*` environment and
  its library path;
- a manifest binding each file's digest, path and mode to the tree, pins, lockfile, toolchain,
  flags and image.

The source tree goes to the benchmark machine as `@local` and refs reach machines today, kept
per tree and sent as a delta. Files a measurement reads from the tree, its scripts, configs and
data, are there without being declared.

## Running it on the benchmark machine

The measured step runs its own command, unchanged. Its `PATH` puts a `cargo` first that answers
from the launch plans: `cargo bench --bench x -- args` runs the staged `x --bench args` from its
package root with cargo's environment, as cargo would. An invocation that matches no plan, or
that would compile anything, exits 78 before running, naming the invocation. Nothing is ever
compiled under the exclusive lock.

Paths compiled into a binary, `env!("CARGO_MANIFEST_DIR")` and `OUT_DIR` among them, name the
compile machine's tree. The tree and the sparse target therefore sit at the same absolute path
on both machines (see the open question below).

Staging runs as a shared job on the benchmark machine before the first measurement: it checks
every digest, that every library resolves, and that glibc, the loader and the card's driver
accept the binaries, as the pilot's stage does. The measurement checks the staged digests again
under its lock and exits 78 if anything moved.

## When a call cannot split

A call splits when every cargo invocation of its measured steps matches a launch plan its build
captured. Otherwise it runs whole on the benchmark machine, as every call does today, and the
summary says which step kept it there and why. A recipe can say `split = false` to skip the
attempt. Nothing is refused for not splitting.

Known cases that stay whole until they are handled: a doctest, which rustdoc compiles while
testing; a measured step that calls cargo by an absolute path; a step that compiles depending on
what it measured.

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

The dibspatcher runs the same planner on ordinary calls and batches, not hand-written splits. A
submission stores what a detached batch carries today: the call or batch text, the sealed
`@local` trees and pins, the caller's recipe overrides, label and identity. Every job of a call
gets an attempt id, not only the last one, so a worker that resumes asks each runner about the
job it may have started instead of starting another. A kill stops new launches, then every open
job on every machine. A detached batch whose steps span machines is driven by the dibspatcher;
one on a single machine is still driven there.

## Stages

Each ships and is tested on its own:

1. **Roles.** The team inventory, roles, and placement of whole calls by their most demanding
   job. Shared work leaves the benchmark machines at once.
2. **Ordinary work through the dibspatcher.** Calls and batches as above, with per-job attempt
   ids and records, still one machine per call.
3. **Splitting.** Capture, bundles of several executables, direct transfer, staging, the cargo
   on the benchmark side, A/B across machines, and the fallback.
4. **`with` servers** split the same way, their servers' binaries carried like any other.

## Open questions

- **The common path.** Either every machine in the pools keeps its trees under one absolute
  path, set when it is provisioned, or the measured step binds the compile machine's path over
  its own in a mount namespace, as builds in an image already do. The first adds nothing to the
  measured process; the second needs nothing from provisioning.
