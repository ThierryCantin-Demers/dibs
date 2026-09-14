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
- `dibs --out <job>` a job's whole log, while it runs or for two weeks after; `dibs --out`
  lists the running ones.
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
- `--device <alias>` runs the job on that card and nothing else. It works with `dibs run` and with
  recipes. **A benchmark on a multi-GPU machine that names no card is not reproducible**,
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
- `dibs bench ... --dry-run` prints which card it would use before anything runs. On a
  measurement worth keeping, look at that line first.
- Do not pass `CUDA_VISIBLE_DEVICES` yourself. dibs sets it, from the alias, resolved on the
  machine at the moment the job starts. Setting it by hand with a bus id looks like it works
  and does nothing: that variable takes an index or a `GPU-<uuid>`, and it ignores anything
  else rather than failing.

### Recipes: prefer one where a repo has it

- `dibs list <repo>` says what a repo defines. `dibs <verb> <repo>@<ref> <recipe>` runs
  it, where verb is `build`, `test` or `bench`.
- Use it in preference to writing a command by hand, because it does five things you would
  otherwise each do differently: it fetches and creates the worktree, exports one build cache
  per repo, derives a stable label, keeps the whole output on the machine, and records which commit of every repo was actually built.
- **The step says which lock it takes**, so a recipe's build runs shared and only its
  measurement runs exclusive. That is the build/measure split made structural instead of being
  a rule you have to remember.
- **`<repo>@local` sends your working tree**, uncommitted changes and all, instead of fetching a
  ref. Use it rather than hand-rolling a sync and a build for a branch you have not pushed:
  refusing to push a perf branch to measure it is reasonable, and the hand-rolled version loses
  the per-tree build cache, the recorded revision and the lock split at once. It follows the
  repo's ignore rules, so no `target` and no `.git` make the trip, and the record names the
  exact tree by content so two runs are comparable only if it matches.
- `dibs runs [label]` is what was actually measured: the commit of every repo, the
  isolation, the time. It also says when a label's recipe has changed, because two procedures
  under one name are two histories, and comparing across them is the mistake the record exists
  to prevent.
- Recipes come in three layers, each overriding the last: bundled with dibs, then a
  repo's own `.dibs.toml`, then `~/.config/dibs/recipes/<repo>.toml`. The bundled ones mean a
  new person has working recipes with no setup; local config is where one lives while it is
  still moving, so it can be iterated on without a pull request against a shared repo.
  `dibs list` says which layer each came from. The run record carries the procedure itself,
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

**One background call per piece of work, not per `dibs` command.** A completion does not
merely hand back a result: it wakes the agent, which re-reads its entire context before it can
look at that result and answer. One job therefore costs three turns whatever it returns, so
launching a hundred jobs one at a time costs three hundred turns at full context. That pattern
alone has been most of a day's token budget, for no benefit at all: the machine did the same
work either way.

Put the whole sequence in one script, launch that script once, and be woken once:

```bash
dibs 'cargo build --release --bench reduce'  || exit 1
dibs --bench 'cargo bench --bench reduce'    || exit 1
echo "both done"        # the one thing you will read when it wakes you
```

**Each step stays its own `dibs` call inside that script.** Do not collapse the sequence into
`dibs 'build && bench'` to save a call: that holds one lock for both, which is the compile
inside the exclusive lock that the split above exists to prevent. The saving is in how many
times *you* are woken, never in how many locks are taken.

**Steps on different machines may overlap** with `&` and a `wait` inside the script. That is
not detaching, because the harness still owns the script and killing it kills everything under
it. Steps on the same machine stay in order.

**Do not batch across a decision.** If a later step should only run depending on what an
earlier one *said*, you will not see the earlier answer until the whole script is done, and
the rest will have run for nothing. Two scripts with a look in between is six turns and still
far cheaper than one per job. When you can state the criterion up front, put it in the step
itself and let a non-zero exit stop the rest.

**Never `sleep`, anywhere, including inside a command sent to the machine.** Locally it blocks
your turn so nothing can steer you. Remotely it is worse: a sleep inside a job holding the
exclusive lock stalls every other person for its whole duration. To wait for something to be
ready, block on a fifo rather than on a clock: `mkfifo f`, start the work, then `read -r _ < f`.

**Split building from measuring.** A `--bench` that begins with `cargo build` holds the whole
machine for minutes doing something that tolerates neighbours perfectly, and everyone else ends
up queued behind a compile rather than behind a benchmark. Two calls: `dibs 'cargo build ...'`
under the shared lock, then `dibs --bench 'cargo bench ...'` for the measured run. This
includes `cargo bench --no-run`, which is a build.

**Say who you are with `DIBS_AGENT`, unless you are Claude Code.** Every job records the agent
that started it, so `dibs --status` can say who to go and ask about a job that is holding the
machine, and so stopping someone else's has to be deliberate. Claude Code is read from its
session. Codex publishes no session id and runs all of its sessions through one shell process,
so it can only be identified as Codex, and any other runtime arrives as the unix account, which
on a shared machine is everyone. Export `DIBS_AGENT` once per session, naming the work rather
than the tool: `DIBS_AGENT='cubek reduce sweep'`.

**Label the kind of work, not the run.** `--label cubek-gemm`, never `--label run3`. The label
is the key the duration history is filed under, so it must be the same every time you run that
kind of work. A label used once files its timing where nothing will ever look it up, which is
why the ETAs are useless when people get this wrong.

**Scratch goes under `$DIBS_SCRATCH` on the machine, never `/tmp`.** That `/tmp` is a small
in-memory filesystem shared by everyone, and one build tree in it fills it for all of us. A
full one breaks every command on the machine, including the ones for finding out why.

**Read the trailer, not the exit code and not the output.** Every job ends with one on stderr:
`job <id>  <mode>  <label>  queued Ns  ran Ns  exit N  by=command|dibs  built=N|nothing`, then
the log path and the `dibs --out <id>` that reads it. `by=dibs` means dibs produced the exit
(69, 70, 71, 75, 124), `by=command` means your command did.

**A job's stdout is a digest**: its first and last 20 lines and a count of what was left out.
Do not pipe a dibs call through `tail`, `head` or `grep`: it is already bounded, and a filter
replaces the exit status with its own. The whole output is kept on the machine for two weeks,
so anyone can read it with `dibs --out <id>`, and `--stream` gives the whole stream inline when
you actually need all of it. Do not redirect inside the command to keep a log: that is done.

**Read `built=` before you read the numbers.** `built=nothing` means cargo finished having
compiled no crate, so a measurement after it measured the previous binary. A shared target
directory, a copy that preserved mtimes and a stale worktree all produce that.

### Exit codes

- **69** the machine is unreachable. Tell the user, do whatever does not need it, and do not
  retry in a loop.
- **70** its scratch filesystem is full. Tell the user. Never delete anything on a shared
  machine to make room.
- **71** the lock directory cannot be written, so no lock could be taken and **nothing ran**.
  A sandboxed shell is the usual cause. Do not work around it by pointing `DIBS_LOCK_DIR`
  somewhere writable: a lock in a directory nobody else uses excludes nobody, which is worse
  than not running. Tell the user.
- **75** it was busy and you had passed `--wait`.
- **124** the command overran `--max` and was killed while holding the lock.

### The account is deliberately unprivileged

Jobs run as a user with no `sudo` and no ability to touch anything outside its own home. If
something genuinely needs more than that, **ask the human** rather than working around it. A
permission error is the system working, not an obstacle to route around.
