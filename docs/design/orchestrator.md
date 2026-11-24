# The orchestrator: work that outlives every caller

`dibs-orchestrator` is a service on a machine that stays up. Coworkers hand it work over ssh with
`dibs orch`, and it makes the dibs calls itself, so a laptop that sleeps, leaves or loses its
network stops nothing. It holds no lock of its own: every lock is still a runner's, taken by an
ordinary dibs call the orchestrator makes.

## Reaching it

Each coworker's key on the orchestrator's account has a forced command,
`dibs-orchestrator serve <coworker>`. The name comes from the key, never from the request. The
forced command reads `SSH_ORIGINAL_COMMAND` and allows two things:

- `dibs-orchestrator api`: one JSON `Ask` on stdin, JSON `Reply` lines on stdout, the last an
  `End` with the exit. The API is versioned (`dibs_format::orchestrator::VERSION`); a version it
  does not speak is refused, not guessed at.
- `rsync --server` writing into `staging/<coworker>/stage/<key>/`, with a short list of options
  and no symlinked component on the way. This is how `@local` trees arrive.

## Durable state

One SQLite store in WAL mode holds submissions, their planned steps, every attempt at a step, the
pools and the bundles attested. A submission's key is chosen by the client and unique per
coworker, so a client that lost its connection after sending submits again under the same key
and gets the same submission back.

## Workers

Each submission is driven by one worker process, started as a systemd user unit (or in a session
of its own) so it outlives the API call. It holds `subs/<id>/worker.lock` for as long as it
drives, and it claims the submission in the store, which bumps its generation. Every attempt it
launches is checked against the epoch and that generation first, so a worker that lost its claim,
or one from before a restore, launches nothing more.

`dibs-orchestrator resume` starts a worker for each unfinished submission that nothing holds. It
is what runs after the service account's manager starts, after a crash, and after a restore.

## Attempts are admitted once

A step's call carries `DIBS_ATTEMPT=<submission>.<step>.<k>`. The runner admits an attempt id
once: the first call creates `$DIBS_SCRATCH/attempts/<id>` and runs the job; any later call with
that id runs nothing and says how the first went, `Record::Admitted`:

- `ended`, with the trailer the runner kept beside the job before reporting it;
- `running`, while the job still holds its receipt's lock;
- `lost`, when its runner went without a trailer, `started` once the job held the machine's lock.

A worker that finds an attempt open in the store, because the worker before it died, asks again
under the same id instead of starting a new one. Builds, transfers and checks run again after a
`lost`; a measurement never does, and its step ends `unknown`.

## Splits

A split recipe builds on the compile pool and measures on the GPU pool, as four steps:

1. **build**: `dibs shell <repo>@local` on a compile machine, with the profile's `RUSTFLAGS`. The
   runner takes the executable cargo reports, copies it into `$DIBS_SCRATCH/bundles/<name>` with
   a manifest (digest, toolchain, lockfile, flags, tree, job).
2. **relay**: the bundle comes back to the orchestrator, is checked against its manifest, kept by
   digest and attested against its inputs, then sent to the GPU machine. A build that compiled
   nothing is accepted only when an earlier build of the same inputs attests what it left.
3. **stage**: a shared job on the card checks the bundle's digest, that its libraries resolve, and
   records the C library and driver.
4. **measure**: `--bench --device <card>`, the runner refusing the bundle (exit 78) unless its
   digest still matches, and exporting `DIBS_BUNDLE`.

Split recipes live in the orchestrator's config, `splits/<repo>.toml`:

```toml
[split.mnist-epoch]
label = "mnist-epoch"
device = "gpu:card"
check = "grep -rq MNIST_ARTIFACT_DIR examples/mnist/src"
[split.mnist-epoch.params]
backend = { choices = ["cuda", "vulkan"], default = "cuda" }
epochs = { default = "1" }
[split.mnist-epoch.build]
package = "mnist"
kind = "example"
name = "mnist"
cargo = "build --release -p mnist --example mnist --features {backend}"
[split.mnist-epoch.run]
env = { MNIST_NUM_EPOCHS = "{epochs}" }
command = "MNIST_ARTIFACT_DIR=$TMPDIR/mnist-$DIBS_SUBMISSION $DIBS_BUNDLE/mnist"
```

## Backup and restore

`dibs-orchestrator backup <file>` copies the store consistently while it is in use.
`dibs-orchestrator restore <file>` sets the current store aside, puts the backup in its place
under a new epoch and resumes: each attempt the backup left open is asked about under its own id,
so work that finished after the backup is learned from its runner rather than run again.
