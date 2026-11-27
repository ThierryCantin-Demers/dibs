# The dibspatcher: work that outlives every caller

`dibspatcher` is a service on a machine that stays up. Coworkers hand it work over ssh with
`dibs dispatch`, and it makes the dibs calls itself, so a laptop that sleeps, leaves or loses its
network stops nothing. It holds no lock of its own: every lock is still a runner's, taken by an
ordinary dibs call the dibspatcher makes.

## Reaching it

Each coworker's key on the dibspatcher's account has a forced command,
`dibspatcher serve <coworker>`. The name comes from the key, never from the request. The
forced command reads `SSH_ORIGINAL_COMMAND` and allows two things:

- `dibspatcher api`: one JSON `Ask` on stdin, JSON `Reply` lines on stdout, the last an
  `End` with the exit. The API is versioned (`dibs_format::dibspatcher::VERSION`); a version it
  does not speak is refused, not guessed at.
- `rsync --server` writing into `staging/<coworker>/stage/<key>/`, with a short list of options
  and no symlinked component on the way. This is how `@local` trees arrive. Before git runs in
  one, its config is replaced by the service's own, keeping only how to read the repo and its
  origin, and its hooks are removed, so nothing sent runs a program here. A tree that is not
  what the client sealed is refused and its stage cleared, so the next send under that key does
  not carry the files an earlier one left. The stage stays locked from its seal until its
  submission is stored, and one a submission was accepted from is never cleared.

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

`dibspatcher resume` starts a worker for each unfinished submission that nothing holds. It
is what runs after the service account's manager starts, after a crash, and after a restore. A
follow, a kill or a submission sent again does the same for its own submission.

## Attempts are admitted once

A step's call carries `DIBS_ATTEMPT=<submission>.<step>.<k>`. The runner admits an attempt id
once: the first call publishes `$DIBS_SCRATCH/attempts/<id>/`, its receipt already locked and
naming the job, and runs the job; any later call with that id runs nothing and says how the first
went, `Record::Admitted`:

- `ended`, with the trailer the runner kept in the attempt's directory before reporting it;
- `running`, while the job still holds its receipt's lock;
- `lost`, when its runner went without a trailer, `started` once the job held the machine's lock.

The attempt's directory is not swept with the job's log, so it answers for as long as it is
asked.

The store closes an attempt only in the transaction that records its step's outcome, or to make
the next attempt after one that says to. A worker that finds an attempt open, because the worker
before it died or a call heard nothing back, asks again under the same id instead of starting a
new one. Builds, transfers and checks run again after a `lost`; a measurement never does, and its
step ends `unknown`. A killed submission's worker only asks after its open attempt
(`DIBS_ATTEMPT_LOOKUP=1`, which the runner answers without ever admitting it), stops a job an
earlier worker's call still runs with `dibs --kill`, and records the kill once the job is seen to
end.

## Splits

A split recipe builds on the compile pool and measures on the GPU pool, as four steps:

1. **build**: `dibs shell <repo>@local` on a compile machine, with the profile's `RUSTFLAGS`, in the
   profile's image when it names one (`docs/design/build-images.md`),
   writing cargo's JSON report of the build to `$DIBS_CARGO_REPORT`. The runner takes the one
   executable that report names, copies it into `$DIBS_SCRATCH/bundles/<name>` with a manifest
   (digest, toolchain, lockfile, flags, tree, job), and builds nothing after the job.
2. **relay**: the bundle comes back to the dibspatcher, is checked against its manifest, kept by
   digest and attested against its inputs, then sent to the GPU machine. A build that compiled
   nothing is accepted only when an earlier build of the same inputs attests what it left.
3. **stage**: a shared job on the card checks the bundle's digest, that its libraries resolve, and
   records the C library and driver.
4. **measure**: `--bench --device <card>`, the runner refusing the bundle (exit 78) unless its
   digest still matches, and exporting `DIBS_BUNDLE`.

Split recipes live in the dibspatcher's config, `splits/<repo>.toml`:

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

`dibspatcher backup <file>` copies the store consistently while it is in use.
`dibspatcher restore <file>` is refused while any worker or call still uses the store,
since each keeps writing to the store it opened: they hold `store.gate` shared, and the restore
holds it alone. It reads the backup without changing it, copies it beside the store
under a new epoch, then sets the current store aside, puts the copy in its place and resumes: each attempt the backup left open is asked about under its own id,
so work that finished after the backup is learned from its runner rather than run again.
