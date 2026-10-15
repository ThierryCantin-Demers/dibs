# A batch that outlives the computer that sent it

`dibs batch --detach <file>` drives a batch on the machine its steps go to. The guide's section
"A batch that outlives this computer" describes it as it is; this page is why it is built this
way.

## Why

A batch's driver is the `dibs batch` process, and its steps die with it (`history/batch.md`). That
is right for a laptop that stays put, and wrong for the run that motivated this: a three-hour
serving benchmark on a rented machine, started from a laptop whose person then had to leave. The
laptop slept, the running step was stopped, and the four after it never started.

## What changes, and what does not

`history/batch.md` says a batch adds no state on any machine and its driver is a client-side
process. A detached batch reverses the first half for itself only: what its driver reads is sent
to the machine, under `~/.cache/dibs/detached/<id>`, and the driver runs there. The second half
holds where it matters. The driver is still a `dibs batch` process, each step still an ordinary
dibs call it makes, and the lock is still an open descriptor held by a live process: a driver that
dies takes its steps with it and every lock releases. Nothing new reserves the machine.

## The driver is the client, built there

A step is a whole dibs call: recipes, refs, pins, trees sent from a working copy, services. Only
the client knows how to make one, so the machine builds the client, from source this binary
carries, exactly as it builds the runner: `dibs-runner build-client <hash>`, a shared job, once
per version. The client's source tree carries both provisioning directories, so the client built
there packs both trees again to the same bytes, and its runner and client hashes are this
binary's.

The driver runs with `DIBS_LOCAL=1`, so every step takes its lock there, and with
`DIBS_CLIENT_DIR`, so everything the client keeps lives under `~/.cache/dibs/detached/home`,
apart from anyone's own dibs on that machine and from the runner's files, which read the XDG
variables too.

## What is sent

Each repo a step names, whole: its files as its ignore rules leave them, then its git directory,
then, for a worktree, its own HEAD and index over the repo's. The driver resolves refs, `@local`,
and a ref the machine cannot fetch there exactly as this computer would, because it has the same
repo. A repo named by a path is refused, since a path means nothing there; `.` is the repo the
batch runs in, and the driver starts inside its copy. Then the recipes, the inventory with
`machines.d`, the series file, and `detach.json`: the driver's machine, and who sent it.

Each repo goes into the batch's own directory, never a shared one, so two detached batches never
see each other's trees. When a batch ends its repos become `last/<repo>`, and the next batch's are
sent with `--copy-dest` against them, so only what changed travels. Transfers use `--checksum`
rather than mtimes, which a sent tree's working copy never builds from.

A tree sent on from the machine's copy of a working tree is keyed by the path it was sent from
(`DIBS_SENT_FROM`), not by where the copy is. A key per batch would give each one a cold tree and
a new target directory, and a rented machine's disk fills.

## Following

The sending call stays, as `dibs batch` does, and prints the summary when the batch ends. It
waits there by taking the driver's record lock shared, which the driver holds exclusively for its
whole life: no polling, and a crashed driver releases it too. What comes back is one JSON document:
the summary, each step's output, the run records and the series file. The records are appended
here once; series lines the batch changed replace this computer's for the same label and machine.

A call cut off exits 69 and says so. `dibs batch --follow <id>` follows again from this
computer's record of where the batch went. `dibs --kill <id>` finds no driver here, so it stops the
batch on the machines, which also stops the driver there.

## Refused

A batch whose steps go to more than one machine, or to none it names. A step that acts on this
computer: `--sync`, `--hold`, `--fetch`, `--check`, `--update`, `dibs with` without `--there`,
`--artifacts <dir>`, `--root`. A detached batch across machines would need an always-on host to
drive it; none is assumed.
