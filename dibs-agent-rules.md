Paste this into your `~/.claude/CLAUDE.md`. It is what tells your agents how to use the
shared benchmarking machine, and most of it exists because someone already got it wrong.

---

## Dibs, the benchmarking machine

- Anything measured runs on the benchmarking machine, never on this laptop: a laptop throttles,
  shares memory bandwidth with everything else running, and its GPU timings are noise.
- The machine is **shared with other people**. Every rule below is about not spoiling someone
  else's measurement, and they are not stylistic.
- Set `DIBS_HOST` to the machine you were given, or every call fails: there is no default.
- Never `ssh` the machine directly to do work. Everything goes through `dibs`, which takes a
  lock. An unlocked command ruins whoever is benchmarking at the time.

### The commands

- `dibs <command>` for builds, tests and inspection: **shared**, several people at once.
- `dibs --bench <command>` for anything timed: **exclusive**, nothing else runs, including
  other people's builds. A compile running beside a benchmark spoils it as surely as a second
  benchmark would.
- `dibs --status` who holds it, who is queued, and roughly how long. Never blocks.
- `dibs --peek <command>` looks at the machine without taking the lock. Free things only:
  `ps`, `nvidia-smi`, `ls`, `tail`, `git status`. It runs *beside* whatever is being measured,
  so anything that costs CPU or IO is charged to that benchmark. When in doubt use the shared
  lock: queueing costs you nothing, and ruining a twenty-minute sweep costs someone everything.
- `dibs --out` what a running job is writing, if it redirected into a file.
- `dibs --log` what has run recently, and what was killed.
- `dibs --help` the rest, including `--sync` for copying files and `--kill`.

### When there is more than one machine

- `dibs --machines` says which ones are known and which is the default; `dibs --on <machine>`
  sends one call to a named one. Without `--on` the default is used, so you rarely need it.
- A machine can be marked `measure = false`, and `--bench` refuses it outright. That is not an
  obstacle to work around: its numbers would not mean anything. Send the benchmark to a machine
  that measures, or run it shared if it was never a measurement.
- `dibs --check <host> --write` records a new machine in the inventory. Run it once per machine.
- With `DIBS_ROUTE=1`, or `dibs --any <command>`, a shared job goes to the least busy machine
  on its own. `dibs --pick -v` shows the ranking without running anything. Benchmarks are never
  routed and you should not try to route one: its history keys on the machine it ran on.
- `dibs --status --all` is every machine at once. Once work is being ranked, `--status` alone
  answers for one machine and that is rarely the question.
- A repo's work sticks to whichever machine holds its build cache, and you do not manage this.
  Do not try to force a build onto an idle machine to make it finish sooner: the benchmark that
  needs what it built cannot follow it there, and would compile inside its own exclusive lock.

### Naming the card, on a machine with more than one

- `dibs --machines -v` lists every machine's cards: the alias to name it by, its bus id, what
  can reach it, and what it is plugged into.
- `--device <alias>` runs the job on that card and nothing else. It works with `dibs` and with
  `dibs-run`. **A benchmark on a multi-GPU machine that names no card is not reproducible**,
  because which card the runtime picks is not yours to decide and is not recorded anywhere.
  dibs says so when you do it; it does not stop you, because a build does not care.
- Two runs under one label have to name the same card, or their numbers are not comparable and
  nothing about the two numbers says so. dibs refuses the second one and tells you what the
  first ran on. If you mean to move a label to another card or machine, say
  `--new-series`: its history starts again rather than mixing the new numbers into the old.
- Two cards of the same model are told apart by their slot, so a machine with a matched pair
  can still name either one. A card the machine cannot answer for is refused rather than run
  unpinned, because a job that measured whichever card came first and reported it under the
  name you asked for is worse than one that did not run.
- `dibs-run ... --dry-run` prints which card it would use before anything runs. On a
  measurement worth keeping, look at that line first.
- Do not pass `CUDA_VISIBLE_DEVICES` yourself. dibs sets it, from the alias, resolved on the
  machine at the moment the job starts. Setting it by hand with a bus id looks like it works
  and does nothing: that variable takes an index or a `GPU-<uuid>`, and it ignores anything
  else rather than failing.

### Recipes: prefer `dibs-run` where a repo has one

- `dibs-run list <repo>` says what a repo defines. `dibs-run <verb> <repo>@<ref> <recipe>` runs
  it, where verb is `build`, `test` or `bench`.
- Use it in preference to writing a command by hand, because it does five things you would
  otherwise each do differently: it fetches and creates the worktree, exports one build cache
  per repo, derives a stable label, redirects the output to a file so anyone can read it with
  `dibs --out`, and records which commit of every repo was actually built.
- **The step says which lock it takes**, so a recipe's build runs shared and only its
  measurement runs exclusive. That is the build/measure split made structural instead of being
  a rule you have to remember.
- `dibs-run runs [label]` is what was actually measured: the commit of every repo, the
  isolation, the time. It also says when a label's recipe has changed, because two procedures
  under one name are two histories, and comparing across them is the mistake the record exists
  to prevent.
- Recipes come in three layers, each overriding the last: bundled with `dibs-run`, then a
  repo's own `.dibs.toml`, then `~/.config/dibs/recipes/<repo>.toml`. The bundled ones mean a
  new person has working recipes with no setup; local config is where one lives while it is
  still moving, so it can be iterated on without a pull request against a shared repo.
  `dibs-run list` says which layer each came from. The run record carries the procedure itself,
  not only its fingerprint, so a recipe that is not in git is still recoverable from the record.
- **If there is no recipe for what you need, use `dibs` directly and tell whoever owns the machine.** A missing recipe
  is a gap worth filling, and the ones that keep coming up are the specification for the next
  one. Do not quietly go back to hand-written commands for something you will do again.

### Rules that are not negotiable

**Always launch it with the Bash tool's `run_in_background` parameter, and never poll it.**
The machine is often busy for twenty minutes or more, a queued job waits that long before it
starts, and a foreground call dies of its own timeout first. When that happens the work simply
never runs, and you report a failure whose cause is invisible. Queueing costs nothing in the
background: do other work and read the result when the notification arrives.

**Never `sleep`, anywhere, including inside a command sent to the machine.** Locally it blocks
your turn so nothing can steer you. Remotely it is worse: a sleep inside a job holding the
exclusive lock stalls every other person for its whole duration. To wait for something to be
ready, block on a fifo rather than on a clock: `mkfifo f`, start the work, then `read -r _ < f`.

**Split building from measuring.** A `--bench` that begins with `cargo build` holds the whole
machine for minutes doing something that tolerates neighbours perfectly, and everyone else ends
up queued behind a compile rather than behind a benchmark. Two calls: `dibs 'cargo build ...'`
under the shared lock, then `dibs --bench 'cargo bench ...'` for the measured run. This
includes `cargo bench --no-run`, which is a build.

**Label the kind of work, not the run.** `--label cubek-gemm`, never `--label run3`. The label
is the key the duration history is filed under, so it must be the same every time you run that
kind of work. A label used once files its timing where nothing will ever look it up, which is
why the ETAs are useless when people get this wrong.

**Scratch goes under `$DIBS_SCRATCH` on the machine, never `/tmp`.** That `/tmp` is a small
in-memory filesystem shared by everyone, and one build tree in it fills it for all of us. A
full one breaks every command on the machine, including the ones for finding out why.

**Redirect long jobs into a file** under `$DIBS_SCRATCH`. Output otherwise goes only to you and
is kept nowhere, so nobody can see how a job is going without asking you. Redirected, anyone
can read it with `dibs --out`.

### Exit codes

- **69** the machine is unreachable. Tell the user, do whatever does not need it, and do not
  retry in a loop.
- **70** its scratch filesystem is full. Tell the user. Never delete anything on a shared
  machine to make room.
- **75** it was busy and you had passed `--wait`.
- **124** the command overran `--max` and was killed while holding the lock.

### The account is deliberately unprivileged

Jobs run as a user with no `sudo` and no ability to touch anything outside its own home. If
something genuinely needs more than that, **ask the human** rather than working around it. A
permission error is the system working, not an obstacle to route around.
