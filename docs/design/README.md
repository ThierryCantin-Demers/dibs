# Design records

What dibs is built on, and why. [The guide](../guide.md) says how to use it.

## Current

- [`architecture.md`](architecture.md): the crates, what each owns, and how a call flows from
  the command line to a job on a machine.
- [`protocol.md`](protocol.md): what the client and the runner say to each other, the files they
  share on a machine, versions and provisioning, the locks and the sweep.
- [`decisions.md`](decisions.md): what is settled, what was deferred, and what is still open, each
  with the measurement behind it. A superseded entry says by what.
- [`machines.md`](machines.md): what each machine should have, the check against what it has,
  and the window that shows the difference.

## History

Records of designs that are built or replaced. Each starts with a status line saying which.

- [`history/agent-interface.md`](history/agent-interface.md): the verbs, recipes and provenance,
  derived from a real job log.
- [`history/batch.md`](history/batch.md): one submission for a whole pipeline.
- [`history/multi-machine.md`](history/multi-machine.md): one machine to several.
- [`history/shared-machine.md`](history/shared-machine.md): sharing one machine through one
  unprivileged account.
- [`history/architecture.md`](history/architecture.md): a layer split rather than a rewrite, and
  what Slurm would take over.
