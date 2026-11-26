# Results

How what comes back is read, and what each exit asks for, cited as `dibs/results::<rule-name>`.

**Upstream files:** none.

## Output

- **Summary:** what a Batch prints when its last Step ends, one row per Step: Machine, the locks
  its Jobs took (`shared+bench` for a build then a Measurement), time, exit, and each Job with
  `by=dibs` when dibs produced the exit and the `built=` its Trailer reported.
- **Digest:** a Job's stdout as a Call prints it: its first and last 20 lines and a count of the
  rest, the whole of it kept on the Machine for two weeks.
- **Build Guard:** what a Recipe does about a stale binary: its build rebuilds a tree that did not
  make the target's last build, and its Measurement exits 78 when another tree built there since;
  a hand-written build that says `built=nothing` compiled no crate, which a shared target, a copy
  that kept mtimes or a stale worktree leaves.

### whole-output-from-out: A Job's output is read with `dibs --on <machine> out <job>`, which for a finished Job shows its tail and the path of the whole log, read for what the tail leaves out, or inline with `--stream`, never kept by a redirect inside the command.

## Exits

### exit-read-by-its-source: An exit means what the table below says only when the Trailer says `by=dibs`, no Job ran, or a Check-in notice came, and is the command's own when it says `by=command`.

| Exit | Means, from dibs |
| --- | --- |
| 2 | refused before it ran, as for a Card no longer in the slot its alias recorded |
| 69 | the Machine is unreachable: off, asleep, behind a login, or its lease ended |
| 70 | the Machine's Scratch is full or over quota, so nothing can run there |
| 71 | the lock directory cannot be written, so nothing ran: a sandboxed shell, or an account outside the Machine's `dibs` group |
| 72 | dibs could not install its runner there, so nothing ran: `dibs --check <machine>` installs it, or its build output says why |
| 75 | the Machine was busy and the Call passed `--wait` |
| 76 | its Batch was cancelled with `dibs --kill <batch-id>` |
| 77 | a Service exited early, was never ready, or cannot be reached from this side, usually the Machine's firewall |
| 78 | a Recipe's Measurement was refused, since another tree built into its target after this one |
| 79 | the Machine's owners closed it, which `dibs status` says with their reason |
| 80 | a Check-in |
| 124 | the Job overran its cap and was killed: `--max`, or else 1800 s for shared work and 7200 s for a Measurement, which a Label's history raises to twice its 90th percentile |

### unreachable-not-retried: A Call that exits 69 is not retried in a loop, and is tried once more after a few minutes only when it says the Machine answered.

### lock-dir-not-moved: `DIBS_LOCK_DIR` never points elsewhere after exit 71, since a lock nobody else uses excludes nobody.

### cancelled-not-rerun: A Batch that exited 76 is not run again unless the person asks.

### unreachable-service-runs-there: A Service the agent's side cannot reach runs its command on the Machine with `dibs with --there`.

### rebuilt-after-78: A Measurement that exited 78 runs again, which rebuilds, and passes `--anyway` only when the binary there is the one meant.

### closed-is-left-closed: No agent runs `dibs --open` on a Machine it did not close, and work for a closed one goes elsewhere or takes `--when-open`.

## After dibs changes

### help-reread-after-change: Once a Call says dibs changed, flags and output are read from `dibs --help`, not from memory.

### friction-reported-too: A `dibs --friction` line also goes where the team tracks dibs reports, with the command, its exit and Trailer, the job id, and what was expected.
