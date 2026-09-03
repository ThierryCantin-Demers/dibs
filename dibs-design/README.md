# Design records

- **`decisions.md`** — what is settled and must not be re-opened, what was considered and
  deferred, and what is still open. Most entries carry the measurement that decided them.
- **`agent-interface.md`** — the verbs, recipes and provenance, derived from a real job log
  rather than guessed. Built.
- **`batch.md`** — one submission for a whole pipeline, because an agent pays turns times
  context and a completion is a turn. Designed, not built.
- **`multi-machine.md`** — one machine to several, one lock to per-device locks. Barely
  started, on purpose.
- **`architecture.md`** — why this is a layer split rather than a rewrite, and what Slurm would
  and would not take over.
- **`shared-machine.md`** — what it takes for several people to share one machine safely.
