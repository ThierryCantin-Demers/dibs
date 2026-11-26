# Batches

How several Calls become one, and what keeps a Job running, cited as
`dibs/batches::<rule-name>`.

**Upstream files:** none.

## Steps

- **Step:** one line of a Batch, which waits for the line before it, or for the Steps its
  `after=a,b` names, or for nothing with a bare `after=`, and stops every Step that waits on it
  when it fails.
- **Overlap:** Steps that wait for nothing in common run at once on different Machines, or on one
  Machine when they are shared and pinned to different Cards or marked `overlap`; a Measurement
  always takes its turn.

### step-is-one-call: A Step is one Call, with no `;`, `&&`, pipe, redirect, `$(...)` or backtick outside quotes, and what runs on the Machine in single quotes.

### loop-is-generated: A Batch with a loop is text a generator pipes into `dibs batch -`, never a script wrapped around Calls.

### cont-on-independent-step: A Step whose failure stops nothing else, one point of a sweep, is marked `cont`.

### overlap-is-light: A shared Step marked `overlap` is light enough to sit beside the Batch's other shared work on its Machine.

### no-batch-across-decision: No Step depends on what an earlier Step said, unless it states that criterion and exits non-zero on it.

### batch-names-machines: Every Measurement Step names its Machine, with `--on` or with `DIBS_ON` exported before `dibs batch` for every Step, while a shared Step naming none is placed.

## Staying alive

- **Driver:** the process that runs a Batch's Steps, on this computer unless the Batch is detached.
- **Detached Batch:** a Batch whose Driver runs on the Machine its Steps go to
  (`dibs batch --detach <file>`), which a sleep or a network change here stops nothing of.

### job-lives-with-its-call: A Job that no Detached Batch runs lives only as long as its Call, which counts as dead once unheard from for two minutes.

### outliving-work-is-detached: Work that must outlive this computer is a Detached Batch, every Step on its one Machine and none acting on this computer.

### cut-off-followed: A Detached Batch whose Call exits 69 is followed in the background with `dibs batch --follow <id>`.

## Check-ins

### stuck-before-kill: A Job is killed at its Check-in, with the `--kill` line it printed, only when what it printed shows it stuck, never because its log is quiet.

### long-call-sets-check-in: A Call known to run long passes `--check-in <time>` or `off`, and a Recipe, a `dibs shell` or a Batch's Steps take `DIBS_CHECK_IN=<time>`.

### paused-batch-followed: A Batch paused by a Step's Check-in is resumed in the background with `dibs batch --follow <id>`, which runs the rest and checks in the same way.

### batch-stopped-by-id: A Batch is stopped with `dibs --kill <batch-id>`, the id `dibs status` and its summary show, never Job by Job.

## Writing a Batch

```
[build]              dibs run --label reduce-build 'cargo build --release --bench reduce'
[bench after=build]  dibs run --bench --label reduce-bench 'cargo bench --bench reduce'
```

A loop, from a generator:

```bash
{
  echo "[build] dibs run --label gemv-build 'cd \$DIBS_SCRATCH/src && cargo bench --no-run --bench gemv'"
  for p in rr rc cr; do
    echo "[gemv-$p after=build cont] dibs run --bench --label gemv-$p 'cd \$DIBS_SCRATCH/src && cargo bench --bench gemv -- $p'"
  done
} | dibs batch -
```
