## Dibs, the benchmarking machines

Everything run on a benchmarking machine goes through `dibs`, which takes a lock. The machines are
shared with other people and their agents, and each rule here exists because a measurement was
spoiled without it.

- Anything measured runs on a benchmarking machine, never on a laptop: a laptop throttles and
  shares memory bandwidth with everything else on it, so its timings are noise.
- Never `ssh` a machine to do work. An unlocked command ruins whoever is benchmarking at the time.
- Never delete anything on a shared machine to make room, and never work around a permission
  error. Tell the person you work for.

### Locks

- `dibs run <command>`, or just `dibs <command>`, for builds, tests and anything else that does
  work: **shared**, several at once.
- `dibs run --bench <command>` for anything timed: **exclusive**. Nothing else runs beside it,
  shared work included, because a compile beside a benchmark spoils it as surely as a second
  benchmark would.
- **Split building from measuring.** The exclusive lock is for what is being measured and nothing
  else. A `--bench` that begins with `cargo build`, or that is `cargo bench --no-run`, holds the
  whole machine for minutes doing work that tolerates neighbours, and everyone queues behind a
  compile. Build with `dibs run`, then measure with `dibs run --bench`, as two calls, never one
  `--bench 'build && bench'`.
- `dibs --peek <command>` takes no lock and runs beside whatever is being measured, which pays for
  it. Only for what is effectively free: `ps`, `nvidia-smi`, `ls`, `tail`, `cat` of a small file,
  `git status`. Anything that compiles, downloads, copies, searches a tree or reads gigabytes
  belongs in `dibs run`, however read-only it looks. A peek that takes more than a moment is
  logged as `peek-slow`.
- `dibs --sync` copies to or from a machine under the shared lock. Never scp or rsync straight at
  it: a copy competes with a measurement for memory bandwidth and writeback. Mark the machine's
  side with a colon: `dibs --sync -a ./tree :~/.cache/dibs/tree` sends,
  `dibs --sync -a :~/.cache/dibs/out ./` fetches, and rsync's own options pass through.
- `dibs --on <machine> --hold <command>` runs the command on your own side while the machine's lock
  is held for it, for work aimed at the machine from outside: provisioning it, or driving it over
  the network. Add `--bench` when nothing else may run there meanwhile. The command is stopped if
  the lock goes first. Do not build this by hand from a job blocking on a fifo and a `--peek` that
  writes to release it: a peek is for looking.
- `--with <name>='<server>'` runs a server on the machine for the length of one call, started under
  its lock and stopped before the lock goes, with `--ready tcp:<port>` or a command saying when it
  can be used. Use it rather than starting a server in the background of a job: one left running
  holds cards and memory while the next person measures. A server that exits early, or is never
  ready, stops the call with exit 77.
- Never write a port number into both the server and the client: two agents serving the same thing
  pick the same one and the second fails. `--port <name>` has the machine pick a free one and
  reserve it, which the server and the command read as `$DIBS_PORT_<NAME>`, a `--hold` command as
  `$DIBS_SERVICE_<NAME>` (`host:port`), and `--ready tcp:<name>` waits for.
- A shared job whose label's history says it is quick goes around a queued benchmark instead of
  waiting it out, delaying it by about a minute at most. Two shared holders while a benchmark is
  queued is that, not a bug.
- **Never `sleep`**, here or inside a command sent to a machine: under the exclusive lock it stalls
  everyone for its whole duration. To wait until something is ready, block on a fifo: `mkfifo f`,
  start the work, then `read -r _ < f`.
- Scratch on a machine goes under `$DIBS_SCRATCH`, never `/tmp`, which is a small tmpfs shared by
  everyone that one build tree fills for all of us. dibs exports it and points `TMPDIR` into it;
  put build trees, logs and binaries in a subdirectory you name.

### One submission, one wake

- **Launch dibs in the background and never poll it.** A machine is often busy for twenty minutes,
  a queued job waits that long before it starts, and a foreground call dies of its own timeout
  first, so the work never runs. In Claude Code that is the Bash tool's `run_in_background`; in
  Codex, an `exec_command` that yields, collected later with `write_stdin`. If you genuinely
  cannot wait, pass `--wait <seconds>` and handle exit 75.
- A job dies with the call that started it, and a call that has not been heard from for two
  minutes, as when a laptop sleeps, counts as dead: the job is stopped and the lock released.
- **One background call per piece of work, not per dibs command.** Every completion wakes you, and
  you re-read your whole context to answer it, so a hundred jobs launched one at a time cost three
  hundred turns at full context. That pattern alone has used most of a day's token budget for
  nothing.
- **Put the sequence in one `dibs batch`.** It takes one dibs call per line, from a file or from
  stdin, and prints one summary when the last step ends:

  ```
  [build]              dibs run --label reduce-build 'cargo build --release --bench reduce'
  [bench after=build]  dibs run --bench --label reduce-bench 'cargo bench --bench reduce'
  ```

- A line is one dibs call and nothing else: `;`, `&&`, a pipe, a redirect, `$(...)` or a backtick
  outside quotes is refused before anything runs. What runs on the machine goes in single quotes,
  which also keeps `$DIBS_SCRATCH` unexpanded until it gets there.
- Loops go in a generator, not in a script wrapped around the calls:

  ```bash
  {
    echo "[build] dibs run --label gemv-build 'cd \$DIBS_SCRATCH/src && cargo bench --no-run --bench gemv'"
    for p in rr rc cr; do
      echo "[gemv-$p after=build cont] dibs run --bench --label gemv-$p 'cd \$DIBS_SCRATCH/src && cargo bench --bench gemv -- $p'"
    done
  } | dibs batch -
  ```

- A step waits for the line before it. `[name after=a,b]` waits for those steps instead, and a bare
  `after=` waits for nothing. Steps that wait for nothing in common overlap on different machines
  and take turns on one, so work on two machines is one batch. A failed step stops the rest; mark
  `cont` on a step whose failure should not, such as one configuration of a sweep.
- **Do not batch across a decision.** If a later step depends on what an earlier one *said*, you
  will not see that until the whole batch is done. Two batches with a look in between is still far
  cheaper than one call per job. When the criterion can be stated up front, put it in the step and
  let a non-zero exit stop the rest.
- `--on <machine>` binds one call. A step that forgets it goes to the default machine and comes
  back unreachable, which reads as your machine going down. `export DIBS_ON=<machine>` before
  `dibs batch` covers every step, including ones added later.

### Reading what came back

- **Read the summary or the trailer, not the output.** A batch's summary has one row per step:
  machine, lock, time, exit, and each job with `by=dibs` when dibs produced the exit and the
  `built=` its trailer reported. A single call ends with the trailer on stderr,
  `job <id>  <mode>  <label>  queued Ns  ran Ns  exit N  by=command|dibs  built=N|nothing`, then
  the log path. A pipe on your side cannot cut it off, and it carries the real exit.
- **Read `built=` before the numbers.** `built=nothing` means cargo compiled no crate, so a
  measurement after it measured the previous binary. A shared target directory, a copy that kept
  mtimes, or a stale worktree all do that. A recipe guards this itself: its build rebuilds a tree
  that did not make the target's last build, and its measurement exits 78 if another tree has
  built there since.
- A job's stdout is a digest: its first and last 20 lines and a count of the rest. Do not pipe dibs
  through `tail`, `head` or `grep`, which replaces the exit status with the filter's, and do not
  redirect inside the command to keep a log. The whole output stays on the machine for two weeks:
  `dibs out <job>` reads it during the run or after, `dibs out` lists the running jobs, and
  `--stream` gives the whole stream inline when you need all of it. Reading a finished job's log
  keeps a copy on your side, which still answers once the machine is gone. Say which job ids a run
  produced, so a person can follow it.

### Status, labels and names

- `dibs status` says who holds a machine, who is queued and roughly how long, and never blocks;
  `dibs --status --all` covers every machine. For a step of a batch it says which step of how many,
  what is still to come on that machine, and how long the batch has left there, queue included.
  **Asked how long your work will take, run it rather than guessing.** The later calls of a script
  are invisible to it.
- **Label the kind of work, not the run**: `--label yield-sweep`, never `yield-sweep-run3`. The
  label is the key durations are filed under, so it stays the same every time that work runs. A
  label used once files its time where nothing looks, and one label over several different
  benchmarks averages them into a number that predicts none of them.
- Each job names the agent that started it, so a person can ask that agent what it is doing.
  Claude Code is identified from its session. Any other runtime, Codex included, must
  `export DIBS_AGENT='<the work>'` once per session as its own statement, naming the work rather
  than the tool: `DIBS_AGENT='cubek reduce sweep'`.
- `dibs --kill <pid>` stops a wedged job, refusing someone else's without `--anyone`, and
  `dibs --log [n]` shows what ran, what it cost and what was killed. Both ignore the lock, so a
  stuck machine can still be freed.
- `dibs --kill <batch-id>` stops a whole batch, the id being the one `dibs status` and the summary
  show. Where its driver runs, nothing more starts and it prints its summary; from anywhere else
  every machine stops that batch's jobs and refuses its later steps, which stops the driver too.
- `dibs --watch` is for a person at a terminal. A backgrounded job tells you when it is done, and
  watching costs the machine.

### Machines

- `dibs --machines` lists the known machines and the default. `dibs --check <host>` says whether a
  machine is usable and what is in it; run it before first use and read its warnings, not only its
  exit code, and `--write` records the machine.
- A machine marked `measure = false` refuses `--bench`. That is not an obstacle to route around: its
  numbers would mean nothing. Send the benchmark to a machine that measures, or run it shared if it
  was never a measurement. Such a machine is still good for builds and tests through `--on`.
- `dibs --any <command>`, or `DIBS_ROUTE=1`, sends a shared job to the least busy machine, and
  `dibs --pick -v` shows the ranking. Benchmarks are never routed: their history keys on the
  machine they ran on.
- A repo's work sticks to the machine holding its build cache. Do not push a build onto an idle
  machine to finish sooner: the benchmark that needs what it built cannot follow it there, and
  would compile inside its own exclusive lock.
- The same commands work on the machine itself; dibs recognises it and locks locally.

### Cards, on a machine with more than one

- `dibs --machines -v` lists each machine's cards by the alias to name them with.
- `--device <alias>` runs a job, or a recipe, on that card only. **A benchmark on a multi-GPU
  machine that names no card is not reproducible**, because which card the runtime picks is
  recorded nowhere.
- Each machine keeps its own series of a label, since numbers from two machines never compare.
  The first run of a label on a machine says on stderr where its other series are: read it, since
  a forgotten `--on` is exactly that line. Keep the plain label for new work; `-kd` and `-mg`
  suffixes are not needed.
- On one machine, every run under one label must name the same card, so dibs refuses a second card
  and says what the first was. To move a label to another card on purpose, pass `--new-series`:
  its series on that machine starts again instead of mixing. A recipe takes it too, and is checked
  before it builds anything.
- `dibs bench ... --dry-run` prints which card it would use. On a measurement worth keeping, read
  that line first.
- Never set `CUDA_VISIBLE_DEVICES` yourself. dibs sets it from the alias, resolved on the machine
  when the job starts; a bus id there is silently ignored rather than refused.

### Recipes: use one where a repo has it

- `dibs list <repo>` says what a repo defines, and `dibs build|test|bench <repo>@<ref> <recipe>`
  runs one. Prefer it to a hand-written command: it prepares the worktree, keeps a build cache per
  tree, derives a stable label, keeps the output, and records the commit of every repo it built.
- Each recipe step names its lock, so the build runs shared and only the measurement exclusive.
- A recipe may take values: `dibs bench <repo>@<ref> <recipe> --backend vulkan --samples 30`.
  `dibs list <repo>` prints what each one takes, with its default and its choices. Use them rather
  than copying a recipe's command out to change one thing: the run is still recorded, still
  labelled, and the record carries the values. A value outside the choices, or a name the recipe
  does not declare, is refused here before anything is sent.
- **A sweep is one call**: `--sweep <name>=<a,b,c>` runs one point per value, as a single batch
  with one summary, so a sweep wakes you once. Never write a loop of dibs calls for it. `--<name>`
  never splits on commas, so a value that contains one, such as a problem list, stays one value.
  `--reps <n>` measures each point n times after one build, into one record.
- **An A/B is one call**: `dibs bench <repo>@main..local <recipe> --reps 3` measures your tree
  against where it left main, never against main as it is now, which would credit your branch with
  whatever landed since. `@a,b,c` compares any refs in turn. Each arm gets its own tree and target,
  all are built first, then the measurements alternate A B B A. Never hand-write the arms, their
  target directories or the alternation. The run ends with each arm's jobs: read the numbers with
  `dibs out <job>`.
- **Build against an unpushed dependency with `--pin`**: `dibs test cubek@local cuda --pin
  cubecl@local` builds cubek against your cubecl tree, through a `[patch]` dibs writes and then
  checks cargo used. Never sync two trees by hand or `sed` a `Cargo.toml` on the machine.
- A recipe's `artifacts` come back by themselves: each step keeps the files it wrote, the run
  fetches them, and `--artifacts <dir>` puts them in a directory at their paths. Do not add a step
  that copies results into `$DIBS_SCRATCH/out` for a `--sync` afterwards. `dibs --fetch <job>
  [dir]` fetches one job's again.
- `dibs shell` takes `--bench` when the one-off is a measurement, and `--max <seconds>` when it
  would otherwise be killed at the default cap.
- `@local` in place of a ref sends your working tree, uncommitted changes included, following the
  repo's ignore rules. It is how to run a branch you have not pushed, and the only way to run a
  private repo, because the machines hold no credentials. Reach for it before carrying code over by
  hand: a bundle, a tarball or a hand-written sync of a tree is this feature done worse.
- An `@local` tree is reused from run to run, and so is cubecl's autotune store inside it: after
  run, edit, run, the second run reads the first one's winners. A recipe with
  `fresh = ["CUBECL_ENVIRONMENT"]` gives each run its own store and pays for autotune every time,
  so its warmup has to absorb that. A margin smaller than the spread across `--reps` is not a
  result.
- `dibs runs [label]` is what was measured: when and where, every repo's commit, the values, the
  measured step's time and lock, the spread across repeats of one procedure on the same code, and
  whether the label's recipe changed, since two procedures under one name are two histories. A
  failed run is listed only with `--all`. A record names its jobs, so `dibs out <job>` finds the
  log behind a number.
- Recipes come in three layers, each overriding the last: bundled with dibs, the repo's
  `.dibs.toml`, and `~/.config/dibs/recipes/<repo>.toml` for one still being worked out. `dibs list`
  says which layer each came from.
- `dibs with <repo>[@<ref>] <service> -- <command>` where a repo declares its servers: the worktree
  is prepared, they are built under the shared lock and started on the machine, and the command
  runs on your side against them, on ports dibs picked. `dibs list <repo>` says which it defines.
- **If no recipe fits, use `dibs run` and tell the person you work for.** A missing recipe that
  keeps coming up is the specification for the next one.

### When dibs says no

- The first call after dibs has changed says so on stderr, once per session, with the commits that
  arrived. Flags and output you remember may be wrong from then on: read `dibs --help`.
- Exits 69, 70 and 71 are for telling the person you work for, never for working around:
  - **69** unreachable: off, asleep, or its network needs a login. Do what does not need the
    machine, and do not retry in a loop.
  - **70** no room: the machine's scratch is full or over quota, so nothing can run there.
  - **71** the lock directory cannot be written, so **nothing ran**; a sandboxed shell is the usual
    cause. Never point `DIBS_LOCK_DIR` somewhere writable: a lock nobody else uses excludes nobody.
- **75** it was busy and you passed `--wait`.
- **76** its batch was cancelled with `dibs --kill <batch-id>`. It was meant to stop: do not run it
  again unless asked.
- **78** a recipe's measurement was refused: another tree built into its target after this one
  did, so the binary there may be that tree's. Run it again, which rebuilds first. `--anyway`
  measures what is there, and is only for when that binary is the one you mean to measure.
- **124** it overran `--max` and was killed while holding the lock. Without `--max`, a label whose
  history runs past the default is given twice its 90th percentile, and says so when it starts.
