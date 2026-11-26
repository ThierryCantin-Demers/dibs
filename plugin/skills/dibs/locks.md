# Locks

What may run beside a Measurement and how a Job shares its Machine, cited as
`dibs/locks::<rule-name>`.

**Upstream files:** none.

## Copies

- **Sync:** a copy to or from a Machine under the Shared Lock, its Machine side marked by a leading
  colon, with rsync's own options passing through
  (`dibs --sync -a ./tree ':$DIBS_SCRATCH/tmp/tree'` sends, `dibs --sync -a ':$DIBS_SCRATCH/out' ./`
  fetches).

### copy-is-a-sync: Every copy to or from a Machine is a Sync, since a copy competes with a Measurement for memory bandwidth and writeback.

### scratch-quoted: A Sync names Scratch as a quoted leading `$DIBS_SCRATCH`, the only variable it expands on the Machine.

## Servers and outside work

- **Hold:** a Call that runs its command on the agent's side while the Machine's lock is held for
  it, and stops it if the lock goes first (`dibs --on <machine> --hold <command>`).
- **Service:** a server run on the Machine for the length of one Call, started under its lock and
  stopped before the lock goes, ready on `--ready tcp:<port>` or a command
  (`--with <name>='<server>'`).
- **Port:** a port the Machine picks free and reserves for one Call (`--port <name>`), read as
  `$DIBS_PORT_<NAME>` by the server and the command, as `$DIBS_SERVICE_<NAME>` (`host:port`) by a
  Hold, and waited for by `--ready tcp:<name>`.

### outside-work-is-a-hold: Work aimed at a Machine from outside, provisioning it or driving it over the network, is a Hold, with `--bench` when nothing else may run there meanwhile.

### hold-is-not-built: No Hold is built from a Job blocking on a fifo and a Peek that releases it.

### server-is-a-service: A server a Call needs is a Service, never one started in the background of a Job, which holds Cards and memory while the next person measures.

### port-is-picked: Every port a Service and its client share is a Port, never a number written into both.

## Sharing the Machine

- **Bypass:** a shared Job whose Label's history says it is quick, going around a queued
  Measurement and delaying it by about a minute at most.
- **Preemptible Job:** a Job that only compiles, which starts ahead of a queued Measurement and is
  frozen while one queued before it runs.
- **Low-Priority Job:** a Job that starts only once nothing else runs or waits on its Machine, and
  is parked when another Job comes, a Sync from another computer aside, to run again from the start
  once the Machine is idle (`--low-priority`); a Measurement that takes it is never parked once it
  runs.
- **Urgent Measurement:** a Measurement that goes before the queue and stops the shared Jobs on
  its Machine, to run them again from the start once it ends, waiting only for a running
  Measurement on its Cards, or on the Machine where benchmarks lock the whole Machine; one per
  Machine (`--urgent '<why>'`), while an urgent shared Job only goes around the queue.

### wait-is-not-a-fault: A frozen Job and two shared Jobs beside a queued Measurement are a Preemptible Job and a Bypass, never a fault.

### compile-only-is-preemptible: A hand-written `dibs run` or `dibs shell` that only compiles, and runs nothing it built, is marked `--preemptible`, while a Recipe's `cargo build` or `--no-run` step is marked by itself.

### long-work-is-low-priority: Long work that should be in nobody's way and that nobody needs by a time, such as a track that runs for hours, is a Low-Priority Job, and so is a Measurement that can wait for an idle Machine.

## A stuck or full Machine

### orphan-released: A lock `dibs status` says an orphan holds is reclaimed with `dibs --release`, since nothing starts until it goes.

### full-machine-read: A full Machine is read with `dibs --gc --dry-run`, and what fills it is reported to the person, since a cache that looks spare may be the one someone measures against tomorrow.
