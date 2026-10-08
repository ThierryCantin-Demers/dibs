# Architecture

dibs is two programs. The `dibs` client runs where the caller is. `dibs-runner` runs on each
machine, takes the lock there and runs the job. They meet over ssh, or as parent and child on
the machine itself, and speak in typed frames. `protocol.md` is that conversation in detail; this
page is who owns what, and why the line is where it is.

## The crates

- **`dibs-format`**: data, linked by both halves, so each shape has one definition. The request
  and the frames, the status document, the run record, the friction note, the fleet report, the
  line codecs of the lock directory's records, the ids (`JobId`, `Label`, `BatchId`,
  `MachineName`), `Exit`, and time as `Moment` and `Span`. It decides nothing.
- **`dibs-runner`**: the machine half, a binary and a library.
  - `lock/`: the gate and `rw` flocks, and the waiting and holder records.
  - `queue.rs` and `history.rs`: arrival order, durations and the estimates built from them.
  - `job/`: the job's process group, its output, its cap, its services and ports, and the tether
    that stops its group when the runner ends.
  - `session/`: one request in, frames out, one exit.
  - `tree/`: worktrees, seeds, package records, build claims, and the sweep that `dibs --gc`
    reports.
  - `status/`, `probe/`, `settings.rs`: what `dibs status` and `--check` read, and the machine's
    policy.
  - `platform/`: one trait, with Linux (`/proc`) and macOS (libproc, sysctl) behind it.
- **`dibs`**: the client, a binary and the library the viewers link.
  - `cli/`: one grammar from the command line to a typed `Invocation`, and the help text.
  - `inventory.rs` and `placement.rs`: the machines, and where shared work naming none goes.
  - `machine/`: a session with one machine's runner: ssh or a local child, provisioning, frames,
    signals.
  - `call/`: each kind of call: run, peek, hold, sync, kill, out, fetch, gc, check, status.
  - `recipe/` and `execution/`: recipes, and a recipe run's refs, arms, pins, schedule and record.
  - `batch/`: the driver behind `dibs batch`.
  - `records/`: what this computer keeps: runs, friction, series, cache affinity.
  - `fleet.rs`, `reports.rs`, `hook.rs`, `update.rs`: `dibs machines`, the report channel,
    `dibs hook ssh`, `dibs --update`.
- **`dibstop`**: the terminal view. It opens its feeds through the `dibs` library and acts by
  running `dibs`.
- **`dibs-machines`**: the desktop window on `dibs machines`, through the same library. It is a
  workspace member but not a default one, since its toolkit is a long build most installs never
  want.

## Who owns what

The runner owns what every caller of a machine must agree on: the lock and its records, the
duration history and the log, the job's lifetime, the scratch layout and its sweep, and the
machine's settings. Two clients of different versions meet only in those files, so they are
written and read through `dibs-format`'s codecs.

The client owns what is about the caller: the grammar, the inventory, placement, recipes and
batches, the change notice, and the records kept on this computer. A recipe's jobs are calls the
client makes itself, not commands it shells out to.

## A call

1. **Parse.** The command line becomes an `Invocation`. A malformed one is refused with exit 2
   before anything is sent.
2. **Choose the machine.** `--on`, the inventory's only machine, or, for shared work naming
   none, placement by each machine's status and by which machine holds the repo's build cache.
   A benchmark names its machine.
3. **Reach the runner.** On the machine itself the client starts its own binary as
   `<binary> __runner serve <hash>`, which the library it links serves. Elsewhere ssh runs a
   one-line `sh -c` that execs `~/.cache/dibs/runner/<hash>/dibs-runner serve <hash>`.
4. **Provision, when the runner is missing.** That line exits 125, or 126 or 127, with nothing
   said. The client sends the runner's source, which its build script packed and hashed, to the
   newest runner already on the machine, which builds the new version as an ordinary shared job.
   Then the call is made once more.
5. **Serve.** The client sends one `request` frame. The runner queues, takes the lock, lays out
   a recipe's tree if it has one, and runs the job in a process group of its own. Output comes
   back as `out` and `err` frames, the trailer as a `record`, and last the `exit`.
6. **Stay alive together.** The client beats while the job runs. The runner takes the end of
   its stdin, or a lease passed without a beat, as the caller gone, and stops the job. The tether
   stops the job's group if the runner itself dies.

## Versions by hash

A client only ever runs the runner built from its own source. The hash of that source names the
runner's directory on the machine, so two clients of different versions each find their own, and
there is no version field and no negotiation. A frame can change in any commit. What must stay
compatible is only what two versions share: the files on the machine (`protocol.md`).

Each machine builds its runner with its own cargo. `decisions.md` says why the machine half is
built there rather than sent as a script with each call.

## One process, several calls

`dibs`, `dibstop` and `dibs-machines` all link the client library, and the viewers make several
calls at once, one thread per machine. The client keeps two pieces of state for the whole
process:

- **Ctrl-C** belongs to the child while a call runs. Each call defers it, the deferrals are
  counted, and the last to end puts back the handler the first replaced.
- **`TERM` and `HUP`** are passed on to one child per process: the rsync of a `--sync`, or the
  command of a `--hold`. Only the `dibs` command makes those calls, one at a time.

A viewer never provisions, nor does `dibs machines`, since each asks within a bound and a build
would queue. A machine without the runner for this dibs shows "has no runner for this dibs yet,
so it was not asked", and the first call that runs there builds it.
