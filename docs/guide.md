# dibs in detail

The [README](../README.md) says what dibs is and how to start. This is the rest, one section per
feature, with the reasons behind the behaviour that would otherwise look arbitrary. `dibs --help`
lists every flag.

## Setting it up

**What you need first.** An account on the machine you can already `ssh` into with a key, and
that is all: dibs installs nothing on the far side, ships itself over the connection, and needs
no root anywhere. One account shared by everyone is the intended shape rather than a compromise,
because it is what lets one build cache serve the whole team, and because a lock keyed to a uid
tells two people the machine is idle at the same time.

Your own computer needs cargo to build dibs, and ssh, rsync 3 and git; bash runs the steps of a
batch. Neither GNU tools nor a recent bash is needed on your side, macOS included. macOS's own
rsync is too old, so a sync there uses Homebrew's (`brew install rsync`) wherever it is, and says
so rather than settling for the other.

    git clone https://github.com/ThierryCantin-Demers/dibs
    cd dibs && ./install.sh

`install.sh` builds `dibs` and `dibstop` and installs them in `~/.local/bin`, or `$PREFIX/bin`.
`--machines` adds `dibs-machines`, a desktop window on what each machine has against what it
should. The half of dibs it sends to a machine, and the commit it was built from, are built into
the binary: an edit in the clone changes nothing until the next install, and only `dibs --update`
needs the clone to be where it was. Installed over a dibs from before, the bash script or a
binary, it replaces it in one rename, so nothing running sees it half written.

Then record your machine and say where your checkouts live:

    dibs --check you@machine --write           # probes it, writes ~/.config/dibs/machines.toml
    export DIBS_ROOT=$HOME/prog                # where your checkouts live, for bare repo names

The entry is named by the machine's own short hostname rather than by the string you dialled,
so `--on` takes a name and not an ssh address. Run it once per machine, and read what it says
rather than only its exit code: it reports what the machine can do, and warns about the things
that are wrong in ways nothing else would tell you. With one machine recorded, every call goes
there. There is never a default among several, so nothing goes somewhere you did not choose;
the next section says what happens instead. `DIBS_HOST=you@machine` still names the machine of
a setup with no inventory at all.

**The one thing the machine does need.** A recipe prepares a git worktree from a clone at
`~/prog/<repo>` on the machine itself, so each repo you want to build has to be cloned there
once. A machine without it is dropped from that repo's routing rather than sent work it cannot
do, and `dibs --check` lists what it has.

**Which machines.** The half of dibs that runs on a machine is `dibs-runner`, which each machine
builds with its own cargo the first time a client needs it (`dibs --check` builds the first one).
It needs cargo and a toolchain at the workspace's `rust-version`, bash for the jobs, and rsync 3
and git for trees sent there. What differs between operating systems is behind one trait in
`crates/dibs-runner/src/platform/`:

- **Linux**, any distribution, read from `/proc` and `/sys`.
- **macOS**, read through libproc. It cannot see which process holds a lock, so an orphaned lock
  there waits for a kill by hand. The one GPU is recorded with no slot, and naming it pins
  nothing.

**Check it works:**

    dibs status                                # who holds each machine, who is queued
    dibstop                                    # the same, live

`dibs --help` is the full surface. `dibs-agent-rules.md` is the rules your agents follow, and is
the shortest useful description of how to use this well. Load it from your clone rather than
copying it, so a `dibs --update` updates the rules too: a line `@~/<clone>/dibs-agent-rules.md`
in `~/.claude/CLAUDE.md` imports it, and an `AGENTS.md`, which has no imports, can have it
included when that file is generated.

`dibstop [interval]` redraws every 10 seconds unless told otherwise, and `?` lists its keys. It
keeps no state of its own: its feed is `dibs --watch --json`, one connection per machine, and
every action it takes is a `dibs` call, so it cannot disagree with what `dibs status` would say,
and `dibs` has to be on PATH for it. A redraw costs a read of the lock on the far side rather
than a fresh login, which is what makes it safe to leave open beside a benchmark.

## Updating

    dibs --update

It fast-forwards the clone `dibs` was built from, lists the commits that arrived, and reruns
`install.sh` when anything changed or when the running `dibs` was built from another commit. The
reinstall goes where the running `dibs` is installed, so a trial under its own prefix updates
itself and not the one on PATH.

## Where recipes come from

dibs carries no recipes, so it knows nothing about any repo until you describe one. A repo's
recipes come from `~/.config/dibs/recipes/<repo>.toml`, and from a `.dibs.toml` in the repo if it
carries one, the first overriding the second recipe by recipe. `dibs list <repo>` says which each
came from.

The local directory can be a git clone. Keep it in a private repository your team can reach,
since recipes name your repos, and everyone clones it into place:

    git clone <your-recipes-repo> ~/.config/dibs/recipes

`dibs --update` pulls it along with `dibs` itself. An edit is an ordinary commit and push, and a
recipe still being tried out can sit uncommitted in the clone until it settles.

A recipe declares a procedure and names no revisions: the invocation supplies the code, and the
run record captures what it resolved to. Pinned revisions would bind the procedure to a moment
and make it harder to rerun every year. The record carries the procedure itself beside its
fingerprint, so a recipe that only ever lived in your local directory is still recoverable from
it. What a recipe never is, is a spec handed over at the moment of the call: that is as opaque
as the script it would replace.

## A recipe that takes values

A recipe declares its knobs, so one procedure covers a sweep instead of a copy of itself per
value. The names are substituted into every command and into what each step exports:

```toml
[bench.solve]
  [bench.solve.params]
  backend = { choices = ["cuda", "vulkan", "cpu"] }
  samples = { default = "10" }
  problems = { default = "small,large" }

  [[bench.solve.step]]
  lock = "shared"
  run  = "cargo bench --no-run --bench solve --features {backend}"

  [[bench.solve.step]]
  lock = "exclusive"
  env  = { BENCH_SAMPLES = "{samples}", BENCH_PROBLEMS = "{problems}" }
  run  = "cargo bench --bench solve --features {backend}"
```

    dibs bench app@local solve --backend vulkan --samples 30

A bare repo name is the checkout you are in when that checkout is the repo, a worktree of it
included, and otherwise the one under `DIBS_ROOT`: inside a worktree of app, `app@local` sends
that worktree.

`dibs list <repo>` prints what each recipe takes, its default, and its choices where it has any.
A value outside the choices, a name the recipe does not declare, and a parameter with no default
left unset are all refused here, before anything is sent. Only declared names are substituted, so
`${VAR}`, `awk '{print $1}'` and the rest of the shell pass through untouched.

The label does not carry the values, deliberately: one label is one duration history, and a label
per value would predict none of them. The run record carries them, so two points stay
distinguishable in `dibs runs`.

A sweep is one submission:

    dibs bench app@local solve --backend cuda --sweep samples=10,30,100 --reps 2

`--sweep` is repeatable and the combinations are the cross product. The points become a batch of
ordinary calls, one per point, run in sequence because they share a worktree and its build cache,
so you are woken once and get one summary with a row per point. Every point is checked before any
of them is queued.

`--reps` measures a point that many times: the build runs once, each rep runs the recipe from its
first exclusive step on, and one record holds every rep. `dibs runs` gives the median and the
spread across them. A recipe with no exclusive step repeats whole.

The sweep has its own flag rather than splitting `--samples 10,30`, because a value may contain a
comma: `--problems small,large` is one value, and `--<name>` always means exactly one.

## Comparing code

An A/B is one call:

    dibs bench app@main..local solve --backend cuda --reps 3

`A..B` measures B against where it left A, their merge base, so what landed on main since the
branch left it is not credited to the branch. A local branch behind its upstream would put that
point too early, so the upstream is asked too and the later answer wins; the output says which.
Both ends are resolved here, and B is fetched by the commit it resolved to, or sent when the
machine cannot fetch it. `a,b,c` compares any
list of refs in turn, which is a bisect, and `local` can be any one of them.

Every arm is prepared and built before any is measured, each in a tree and a target directory of
its own: a local tree already has one, and a second fetched arm gets `<repo>-arm1` rather than
sharing the repo's. Each rep then measures every arm, the order reversed every other rep, so three
reps of an A/B run A B B A A B. That cancels a drift that is linear in time, such as a card
warming up, which A B A B would credit to B. The run ends with each arm's seconds per rep and the
jobs holding its output, and writes one record naming every arm's revisions, with each step
tagged by its arm and rep. The seconds are the steps' wall time, a first look; the numbers are in
the recipe's own output, which `dibs out <job>` reads.

`dibs shell` takes `--bench` for a one-off that is a measurement, and `--max <seconds>` where the
default cap is too short for it. Named with no repo, it sends the tree it was called from, as
`.@local` would, and inside a worktree a bare name may be the worktree's directory as well as its
repo.

Two things in a recipe are refused when it loads, because both produce a number that looks fine:

- A step naming a relative `target/` path. `CARGO_TARGET_DIR` is redirected per tree, so that
  directory is not the one the build writes, and the step reads whatever an earlier tree left.
- A step that compiles under the exclusive lock, which holds the whole machine for work that
  tolerates neighbours. The message shows the two-step form: build with `--no-run` under the
  shared lock, measure under the exclusive one.

## Building against another repo's tree

    dibs test app@local cuda --pin lib@local

An app that takes a library by git revision sees a change to it only once the change is pushed
and the revision bumped. `--pin <repo>@<ref>` sends that repo's tree as well,
your checkout for `@local` or the ref otherwise, and points cargo at it with a `[patch]`
for every crate of it the lockfile takes from git or crates.io. It is repeatable, and a crate one
pinned repo takes from another is covered too.

The patch goes in `.cargo/config.toml` in the directory above the tree, where cargo reads it
after the tree's own, so the tree stays exactly what was sent. A pinned build still gets a tree
and a target directory of their own, since resolving the patch rewrites the lockfile. After each
build, dibs checks the lockfile: if cargo still takes a pinned crate from where it came before,
most often because the pinned version does not meet the requirement, the step fails with exit 3
rather than measuring the pushed code. The record carries the pinned tree's revision beside the
repo's own, and `--dry-run` says what each pin replaces.

## Refs the machine cannot fetch

A machine holds no credentials and sees only what was pushed, so every ref is looked up here
first. A commit on no branch of `origin` here, or any commit of a repo whose remote refuses an
anonymous read, is checked out in a clone under `~/.cache/dibs/sent` and sent like a local tree,
into a tree on the machine keyed by the commit. That covers each end of a range, each arm of a
list, `--pin <repo>@<ref>` and `dibs with`. The base of a range ending in `local` is always sent,
whatever the remote holds. A ref is the commit as this checkout last fetched it, `origin/<ref>`
where there is one, unless the local branch of that name has commits `origin/<ref>` lacks: then it
is that branch as it stands here, and it is sent. The output names which. Whether a remote needs
credentials is asked at most once a week and kept in `~/.cache/dibs/remotes`.

## Getting files back

A recipe names the files it wants back:

    [bench.breakdown]
    artifacts = ["benchmarks/baselines/*.json", "$CARGO_TARGET_DIR/criterion/**/estimates.json"]

Each step copies the matching files it wrote into its job directory on the machine, at their path
in the tree, or under `target/` for one under `$CARGO_TARGET_DIR`. A file older than the step is
skipped, since trees are reused and a file an earlier run left would come back looking current.
The run then fetches every job's files and keeps them beside the job's log, in
`~/.local/state/dibs/jobs/<job>/artifacts`, and `--artifacts <dir>` copies them into a directory
at their paths, with `<arm>/` and `r<rep>/` added where a comparison or reps would otherwise write
one path twice. `dibs --fetch <job> [dir]` does the same by hand. Fetching takes no lock, like
`dibs out`, so a job's files are capped at 64MB; more than that is a `dibs --sync` under the
shared lock.

## A cache of its own each run

Some tools keep state inside the tree they run in, such as autotune results or compiled kernels.
Recipe steps run inside their tree, so each tree has its own. A fetched ref gets a tree per
commit, so comparing two commits is safe. An `@local` tree is named after the checkout's path, so
run, edit, run reuses it, and the second run reads what the first one stored.

    [bench.solve]
    fresh = ["SOLVER_CACHE_NAME"]

`fresh` names variables that get a value unique to each run, the same in every step of it, and the
record carries the value. Each rep and each arm of a run gets one of its own. It is a value rather
than a directory, since a value is what such a knob takes: a tool that names its store after it
keeps one store per run inside the tree, and the store goes when the tree does.

A fresh store is cold, so every run pays to fill it. The benchmark's warmup has to absorb that or
the first measured iterations include it, and where the store holds autotune results the spread
across `--reps` now includes autotune picking different kernels. A margin smaller than that spread
is not a result.

A new `@local` tree is seeded from a sibling's, as the next section describes, which would hand
it the sibling's store along with its sources. The recipe file says what a new tree of the repo
starts without:

    [tree]
    fresh = ["target/environment"]

Each entry is a path inside the tree, and anything that could reach outside it is refused when the
file loads. `dibs list` shows them.

## Build caching

dibs sets no compiler wrapper and leaves incremental compilation to each profile, so dev builds
are incremental and release builds are not, the same as on a laptop.

sccache was tried and dropped. Its key for a Rust crate includes the target directory's path,
and `SCCACHE_BASEDIRS` does not strip it, so it hits only when a directory is refilled at the
same path. Every `@local` tree has a target directory of its own, so on a new tree it missed on
every crate while still costing incremental compilation, which it refuses to cache.

A new `@local` tree's target directory starts as a reflink copy of the sibling that has built the
most of its `Cargo.lock`, newest first among equals, skipping any a build holds. That sibling's
sources are copied with it, and the sync then rewrites only files whose bytes differ, so an
unchanged file keeps the time its artifacts were built at and only changed crates rebuild. Each target
directory keeps the union of every lockfile built into it in `.dibs-packages`, written only once
a `cargo build`, `test`, `bench`, `run` or `nextest` step exits 0, and each entry carries that
build's toolchain, profile, target, feature flags and RUSTFLAGS, so a debug build never passes
for a release one. A pinned git dependency found in more than one cache directory here, because
its URL was spelled two ways, is sent from each. Dependencies are then reused, and the workspace crates
rebuild because the sync gives every file the current time. Only a filesystem with reflinks,
such as XFS or btrfs, gets the copy, since a full copy per tree would fill the disk.

Commits of a fetched repo share one target directory, and cargo trusts a source file older than
its last compile, so a worktree checked out before another commit built there is handed that
commit's artifacts, with nothing compiled. Each target therefore records which tree made its last
build, and a build from any other tree first dates its own sources now, which rebuilds its
workspace crates and nothing else. A measured step after a build checks, under the exclusive
lock, that its tree still made the last build, and exits 78 if another tree has built there
since: running it again rebuilds first, and `--anyway` measures what is there. A rerun of one
tree that compiles nothing is not refused, since that binary is its own.

A git dependency pinned in `Cargo.lock` that the machine lacks is sent from this machine's cargo
before the build, when this cargo has that commit. A machine holds no credentials, so a private
repo would otherwise fail the build after it had queued. Files are only added, never replaced.

Worktrees and target directories are both collected, by every prepare, local or fetched, across
every repo on the machine. A worktree goes after `DIBS_KEEP_DAYS` (14) unused. A target directory
goes after `DIBS_TARGET_KEEP_DAYS` (5): disk is what runs out first on a machine, and a
compilation cache makes refilling one cheap.

## What is filling the machine

`dibs --gc` lists what is under the machine's scratch directory, each worktree and build cache
with its size and when it was last used, and the job directories and leftover temporary files
counted together. It then removes what is past the clock above, and says how much that was.

`--dry-run` removes nothing and marks what would go. `--days <n>` treats anything unused for n
days as past its clock, for worktrees and caches alike, which is the knob when a machine is full
now. Anything under scratch that dibs did not put there is listed with its size and never
touched: a directory somebody wrote by hand may be the only copy of what they are working on.

It takes the shared lock, because deleting gigabytes is as much IO as writing them, so it queues
behind a measurement rather than competing with one, and it is recorded like any other job.

## A lock with nothing behind it

The lock is held by the workload's own process, so it goes when that process does. A process that
outlives the caller that started it, which is what a killed shell or a crashed ssh can leave,
holds the lock with no holder record to show for it: `dibs status` calls that an orphan and names
what it is, and a caller that arrives meanwhile is told it is queueing behind one rather than
behind a job.

`dibs --release` reclaims it. There is nothing to unlink, the lock being a descriptor, so it ends
the process holding it, having read the lock twice a moment apart first: a client takes the lock
for an instant to test it, and killing a passer-by would be worse than the wedge. It prints what
it stopped, and `dibs --log` keeps it.

## What was measured

Every recipe run appends a line to `~/.local/state/dibs/runs.jsonl`, or `$DIBS_RUNS`: the label, the
repo and the commit of every repo it built, the values, the machine and card, the procedure and
its fingerprint, the batch it was a step of, and for each step its lock, exit, seconds, job,
`built=` and log. A measured step first reads the machine's state into it (CPU governor, kernel,
NVIDIA driver), and the run ends `ok` or `failed`. A run from a git worktree keeps its repo's
label and records the worktree's folder as `variant`, which `dibs runs` prints as `from <folder>`.

`dibs runs [label]` lists them newest first with the date, machine, label, the measured step's time
and lock, and the commits. A failed run is listed only with `--all`. Runs that differ in nothing
recorded are grouped with their median and range, which is the noise a difference has to beat,
and another governor or driver makes another group rather than a wider one. A label whose
procedure changed under the same values is called out with the step that changed.

A label's finished runs are also what every estimate is built from: how much longer a holder has,
when a queued caller starts, whether a shared job is quick enough to go around a queued benchmark,
and the default cap, which becomes twice the 90th percentile where a label's history runs longer
than its mode's default. A recipe files each duration under the fingerprint of the procedure it
ran as well as under the label, so one recipe measured on two backends, or a suite that gained a
step, is not predicted from runs that did different work. With nothing recorded under that
fingerprint the label answers, exactly as it did before, so a sharper key never leaves a job
without an estimate.

## What got in the way

`dibs --friction '<one line>'` records a problem: a flag that is missing, a message that misled, a
bug, anything an agent had to work around. It is one line, in whoever hit it's own words, kept in
`~/.local/state/dibs/friction.jsonl` or `$DIBS_FRICTION` with the session that reported it and the
commit dibs was at.

A team can send those reports to whoever maintains dibs. With `DIBS_REPORTS=<owner>/<repo>` set,
naming a private repo that person can read, each report is also filed there as an issue labelled
`dibs-friction`, through `gh`, and a public repo is refused because a report carries paths and
commands. It is per person: without the variable, nothing leaves the machine. An answer posted on
the issue is printed on the stderr of a later dibs call from the session that filed it, which
checks at most every `$DIBS_REPORTS_EVERY` minutes (5).

The maintainer's side: `dibs --friction --wait` returns when a report or a comment lands, having
read first whatever landed while nothing was listening, so a session can run it in the background
and be woken by it. It listens through `gh webhook forward` (`gh extension install cli/gh-webhook`,
admin on the repo), with no public endpoint. `dibs --friction --reply <n> '<answer>' [--close]`
answers one, and a wait never wakes on its own answer.

`dibs gaps` prints those beside the other thing that means the tool did not fit: the reasons
recorded by `dibs raw --reason` and `dibs shell --reason`, which is what a run that needed no
recipe says for itself. Both are grouped by what was said, and the count leads, because one report
is a nuisance somebody worked around and the same one three times is the specification for a fix.

It is deliberately not a bug tracker. A line here costs the agent nothing at the moment it is
annoyed, which is the only moment it knows, and whether any of it becomes work is read later from
what recurs.

## Batches

`dibs batch <file|->` runs a list of dibs command lines as one submission and prints one summary
when the last step ends, so an agent is woken once for the list instead of once per job. One
step per line, optionally prefixed `[name after=a,b cont]`. A step without `after=` waits for the
one before it, and a bare `after=` waits for nothing; steps that wait for nothing in common overlap
only on different machines. A failed step stops the rest unless it is marked `cont`. Each step's
output is kept under `~/.local/state/dibs/batch/<id>/`, and the summary names each step's jobs for
`dibs --out`, with `by=dibs` and `built=` from their trailers. The
driver owns its steps: however it dies, they die with it and release their locks.

Each step carries the batch's plan to the machine, so `dibs status` shows a job as step k of n,
lists the steps still to come on that machine with what their labels usually take, names those
bound elsewhere, and gives the time the batch has left there. A step whose label has never run
makes that a floor. `dibs --log` tags every event with its batch and step. A list is often
generated rather than written: `./make-steps.sh | dibs batch -`.

A recipe is a batch of its own jobs, and prepares its worktree inside the first job that needs
it: the transfer for `@local`, or the first step at a ref when that step is shared. That is one
round trip and one place in the queue less than a setup job of its own.

## Holding a lock for a command run elsewhere

Some work on a machine does not run on it: a playbook that provisions it, or a client driving it
over the network. `dibs --on <machine> --hold <command>` takes the machine's lock, shared or with
`--bench` exclusive, and runs the command on your side in the foreground, with your terminal, so
prompts and Ctrl-C work. The lock goes when the command ends, with its exit in the trailer and
the log. If the lock goes first, through `--max`, `--kill` or a cancelled batch, the command is
stopped rather than left running unlocked. If your side dies, the machine hears it through the
same channel every job has, and lets go.

## A service for the length of one call

Some work needs a server on the machine that the command talks to: a GPU server a client drives
over the network, or a database a test suite needs. `--with <name>='<command>'` starts one once
the lock is taken and stops it before the lock is released, however the call ends, so it is up
only while something is using it and never while someone else holds the machine. `--ready` says
when it can be used, as `tcp:<port>` or a command that exits 0 once it is, and `--ready-within`
bounds that wait. Its output is kept beside the job's, and the trailer says how it went.

`--port <name>` has the machine pick a free port and reserve it for the call, rather than a
number written into both sides by hand, which two agents serving the same thing collide on. The
service and the command read it as `$DIBS_PORT_<NAME>`, a `--hold` command also gets
`$DIBS_SERVICE_<NAME>` as `host:port`, and `--ready tcp:<name>` means that port.

```
dibs --on box-a --hold --port api \
    --with cuda='target/debug/gpu-server --listen 0.0.0.0:$DIBS_PORT_API' --ready tcp:api \
    -- 'curl http://$DIBS_SERVICE_API/gpus'
```

A client on the machine itself is the same call without `--hold`, and a measured one adds
`--bench`, which is what puts the server inside the exclusive lock rather than beside it. A
service that exits before it is ready, or while the command is still running, stops the command
and the call exits 77 with the end of the service's log, since a client that goes on without its
server produces a failure that reads like the client's own.

A repo that keeps needing the same servers declares them once, beside its recipes, and
`dibs with <repo>[@<ref>] <service> -- <command>` prepares the worktree, builds them under the
shared lock, starts them there and runs the command here against them. `dibs list <repo>` says
which it defines. With `--bench` the command is timed: the build still runs under the shared
lock, then the servers and the command hold the machine alone, and a server whose build cache
another tree has built into since refuses to start, exiting 78, unless `--anyway` says to time
what is there. `--there` runs the command on the machine instead, in the tree beside the servers
with its build cache, reaching them at `127.0.0.1:$DIBS_PORT_<NAME>`: a loopback measurement,
with no network between the two to pay for.

```toml
[service.gpu-servers]
build = "cargo build -p app-gpu-server --features cuda,vulkan"
ports = ["cuda", "vulkan"]

[[service.gpu-servers.serve]]
name = "cuda"
run = "target/debug/app-gpu-server --backend cuda --listen 0.0.0.0:$DIBS_PORT_CUDA"
ready = "tcp:cuda"
```

## More than one machine

`dibs --check <host> --write` records what it finds there as an entry in
`~/.config/dibs/machines.toml`, and `dibs --on <machine>` sends a call to one of them.
`export DIBS_ON=<machine>` does the same for every call that follows, which is the form a step
added to a script later cannot forget.

**There is no default machine.** A benchmark's series belongs to the machine it ran on, so a
default is where a forgotten `--on` starts a series nobody meant, with nothing but a line on
stderr to show for it. So with several machines, a call that names none is one of three things:

- **Shared work is placed**: on the machine holding the repo's build cache, else the least busy
  one, as below. A plain `dibs <command>`, a build or test recipe, and `dibs raw` all are.
- **A status shows every machine**, as `--status --all` does.
- **Anything else is refused**, with the machines to choose from: a benchmark, a benchmark
  recipe, a `--hold`, `--with` servers, a peek, a sync, a kill of a pid, a log, a gc, a check.
  A batch with a benchmark step that names none is refused before any step runs. `dibs out <job>`
  names the machine too, which the trailer's line does for you, unless the log was already read
  and kept here.

One machine in the inventory is no choice at all and is used. `DIBS_HOST` names the machine of
a setup with no inventory, and chooses nothing once there are several.

`dibs --machines` says what is known, `dibs --forget <machine>` drops one, and `dibs status`
shows every machine at once, which is how you find where a job is actually running once work is
being placed. The inventory is not in this repo because it names your hosts.

`dibs --pick -v` shows where shared work would be placed, and why, without running anything.
Machines are ranked on `/proc/loadavg` rather than on what dibs itself holds, because
on a machine someone is working at, most of the competing load was never started through dibs.
A machine marked `workstation = true` is discounted further, since a build that takes every
thread costs whoever works there their editor. That is a property of the machine rather than of
whoever dispatched, so a headless box running the agents is not discounted for it.

**A repo's work goes where its build cache is, even when that machine is busy.** Each machine
reports which repos it has actually built, by a marker cargo writes rather than by the target
directory existing, since preparing a worktree creates that directory whether or not anything
is built in it. A recorded preference in `~/.local/state/dibs/affinity` breaks ties, and a
benchmark's claim is the one that sticks, because a benchmark is the run that cannot move. Only a
recipe run that dibs placed writes it, never one sent with `--on` or `DIBS_ON`, and it is
forgotten after five days, when the machine collects the target directory it named.

Being busy is not a reason to look elsewhere, and this is the part worth understanding: nothing
built on one machine can be used on another, because there is no way to move artifacts between
them. A build placed away from the cache is work thrown away, and the benchmark that follows it
still finds nothing and compiles inside its own exclusive lock, which is the thing splitting
build from measure exists to prevent. Queueing is slower for one job; the alternative is
useless. Only a machine that does not answer at all is given up on.

The load ranking therefore decides one thing: where a repo nobody has built yet should go. Even
there it prefers a machine that accepts benchmarks, because that first build is what decides
where the cache lives, and a machine marked `measure = false` is one no benchmark can ever
follow it to. So a second machine that cannot measure earns its keep on repos that are never
benchmarked, and on whatever you send it deliberately with `--on`. That is a smaller claim than
"it splits the load", and it is the true one until artifacts can move.

**Benchmarks are never routed.** A measurement's duration history keys on the machine it ran
on, so moving one files two distributions under a single label. `--bench` goes where it is
told.

Routing also drops a machine that has no clone of the repo under `~/prog`, because a worktree
is prepared from one and a job sent to a machine without it queues and then fails. Dropped
rather than ranked last: last still wins when it is the only machine that answered. `dibs
--check <machine>` names the repos a machine can build, so a missing clone is something you can
see rather than a machine that quietly never gets that work.

An entry can say `measure = false`, and `--bench` then refuses it. That is for a machine whose
numbers would not mean anything, a laptop most of all: it throttles, it moves, and its iGPU
shares one memory pool with the CPU. `--check --write` sets it when it finds a battery. Such a
machine is still useful for everything that is not a measurement, which is most of what runs.

## Naming the card

A machine with several GPUs has the same problem the lock solves, one level down: two runs of a
benchmark are only comparable if they ran on the same silicon, and which card the runtime picks
is neither the caller's to decide nor recorded anywhere.

`dibs --machines -v` lists each machine's cards, and `--device <alias>` runs a job on one of
them. It works with `dibs run` and with recipes, and `dibs bench ... --dry-run` prints which card it
would use before anything runs.

What that turns into differs per runtime, and none of it is guessable. First, whatever the runtime,
the slot is read back at launch: one that is empty, or holds another model than the inventory
recorded, refuses the job with exit 2 and says what the slot holds now, since every selector below
would otherwise ignore the address and run the job on the first card under the name asked for.
`dibs --check <machine> --write` records the machine as it is.

`CUDA_VISIBLE_DEVICES` takes an index or a `GPU-<uuid>`, never a bus id. Handed one it does not
fail, it ignores the value and leaves every card visible, so a job looks pinned and is not. The
inventory's bus id is resolved to a UUID on the machine at launch. A machine that cannot answer
for the card refuses the job instead of running it unpinned.

Vulkan needs two variables that fight each other. `DRI_PRIME` takes a PCI address and is the
only thing that tells two cards of one model apart, but it is Mesa's and does nothing for the
NVIDIA ICD. `MESA_VK_DEVICE_SELECT` is a layer above every ICD so it does reach NVIDIA, but it
keys on vendor and model, which names both halves of a matched pair. Set together the layer
reorders last and wins, which sends both aliases of a pair to one card while each looks pinned.
So `DRI_PRIME` always, and the model selector only where the model names one card.

Where the model names one card, the layer also hides the others, so a job that enumerates and
takes an index of its own still lands on the card that was named: the guarantee the CUDA
variable gives. A matched pair cannot have it, the selector having no way to name one of the
two, so there `DRI_PRIME` reorders and the other card stays visible.

**One label, one series.** A label is the key a measurement's history is filed under, so two
runs of it are meant to be two samples of one thing. They are not if they ran on different
cards or different machines, and nothing about the two numbers says so. Each machine keeps its
own series of a label: the machine is named on every call and in every record, so numbers from
two machines never mix. The first run of a label on a machine says where its other series are,
with how many runs each has, which is what catches a forgotten `--on`. Within one machine the
first benchmark records which card it ran on, and one on another card is refused, naming the card
it was measured on before, since that is usually a missing `--device` and nothing else shows it.
`--new-series` moves a label to another card deliberately and starts its series on that machine
again, rather than mixing the new numbers into the old. Checked before the run and recorded after
it, so a benchmark that failed claims nothing. A recipe is checked before its build, so a refusal
costs nothing on the machine, and takes `--new-series` the same way.

`--check` also reports what each card is plugged into, walked to the root complex rather than
read off the endpoint: a card with its own bridge reports the width between its die and its own
upstream port, which says nothing about the riser above it. A card reaching the host over fewer
lanes than it can drive is worth knowing about before believing a number that moved data.

## What each machine should have

`--check` says what a machine has. `dibs machines` says what it lacks, against
`~/.config/dibs/fleet.toml` (or `DIBS_FLEET`), which is yours and names your machines, your people
and their keys:

```toml
[person.alice]
keys = ["keys/alice.pub"]            # relative to this file

[machine.box]
provisioned = { by = "ansible", source = "~/prog/box-ansible" }   # or { by = "hand" }
people = ["alice"]                   # whose keys belong in its authorized_keys
login = "keys"                       # or "tailscale", for Tailscale SSH
paths = ["box.local", "box.example.ts.net"]
profiles = ["dibs", "rust", "cuda", "vulkan", "unprivileged"]      # also "metal"
repos = ["burn", "cubecl"]           # default: every repo with a recipes file
# ssh = "user@box.local"             # only for a machine not yet in the pool
```

Each machine is probed at once, a machine in the pool as a shared job under its lock, waiting at
most 30 seconds before it is reported busy instead. The report, or `--json`, says per machine:

- **paths:** whether each name resolves here and answers on port 22.
- **login:** with keys, whether everyone listed has a key there, and any key belonging to nobody
  listed, or to someone not listed for that machine, compared by fingerprint. With Tailscale SSH,
  whether it is on, and any key in `authorized_keys` beside it, which would be a second way in;
  who may log in is then the tailnet's policy, which the machine cannot say.
- **repos:** a clone at `~/prog/<repo>` for each.
- **dibs:** bash 5.1 or newer, flock, GNU timeout, rsync and git.
- **rust:** rustup, stable, and every toolchain the machine's repos pin in `rust-toolchain`.
- **cuda**, **vulkan**, **metal:** the driver and nvcc, the loader, macOS.
- **unprivileged:** the account has no sudo without a password and is in no group that is root in
  all but name.

It exits 1 when anything is missing or a machine could not be probed.

## A machine's own settings

How long a benchmark lets quick jobs go around it, how long scratch is kept and the rest are the
machine's policy, not a caller's: ssh forwards no environment. They live in two files, one
`name = value` a line, each named as its variable without `DIBS_`.

What every account on the machine must read alike lives in `/etc/dibs/runner.toml`, which only
root writes, and nowhere else: one account's `quick = 600` would send its jobs around everyone's
benchmarks.

```toml
patience = 60          # seconds a queued benchmark lets quick jobs go around it
quick = 10             # what a job's own history must say it takes, at most, to go around
bypass = true          # whether quick jobs go around at all
machine_series = false # see below
```

The rest may differ by account, in `~/.config/dibs/runner.toml` in the account dibs runs as,
over the same names in `/etc/dibs/runner.toml`:

```toml
keep_days = 14         # worktrees, job directories and temporary files
target_keep_days = 5   # build caches
ports = "20000-29999"  # what --port picks from
```

Also `idle_after`, `wrote_within`, `peek_warn`, `digest_head`, `digest_tail`, `repeat_window`,
`seed_wait` and `no_children`. The files come first, then the variable, which only a call on this
computer can set, then the default.

Where the lock and the shared files are is not policy: `DIBS_LOCK_DIR`, `DIBS_SHARED_LOCK_DIR`,
`DIBS_SHARED_STATE_DIR`, `DIBS_HISTORY`, `DIBS_LOG` and `DIBS_SCRATCH` come from the environment
alone, which everything that takes the lock on the machine reads, so one account cannot split
who excludes whom. A key a file may not set is named on every call and by `dibs --check`, and
ignored; `dibs --check` also names every line that sets nothing dibs reads, a misspelt key or a
table.

`machine_series = true` has the machine hold each label's measurements to one card, whoever runs
them, from the file `cards` beside its history; without it, each laptop holds only its own runs to
a card. It refuses jobs, so it is off until everyone has updated.

## How the lock behaves

A shared holder blocks a benchmark and a benchmark blocks everything, because a compile running
beside a benchmark contaminates it as surely as a second benchmark would.

`--peek` and `--kill` deliberately ignore the lock. Looking at a wedged machine, and freeing it,
must not require the thing that is wedged. That exemption is the whole risk of `--peek`: it runs
while a benchmark holds the machine, so the cost of what you peek at is paid by whoever is being
measured. Use it only for things that are effectively free: reading a small file, listing a
directory, `ps`, `nvidia-smi`, `git status`. If it compiles, copies, downloads, greps a tree, or
reads gigabytes, it is not a peek however read-only it looks: run it as `dibs <command>` and take
the shared lock, which is what the shared lock is for.

`--watch` keeps one connection open and redraws from the far side, and a redraw forks nothing: it
reads the lock files, walks the holder's own process tree through the kernel's child lists, and
reuses the medians it worked out last time. That is about 2 ms against the 30 ms a `--status` in
a loop spends on a fresh login and a machine-wide process scan. It is for a person at a terminal.
An agent is told when its own job ends and has nothing to watch for.

The lock lives on the machine, not on your side, so it also covers agents running on the machine
itself, and it is held by the workload's own process: it releases when that process exits,
however it exits, with nothing left to clean up. A call made on the machine recognizes it by
hostname and takes the lock locally instead of reaching itself over ssh.

A job runs with `DIBS_SCRATCH` and `TMPDIR` pointing at `~/.cache/dibs` on the machine, because
`/tmp` there is a shared tmpfs under a quota: one build tree in it stops everyone else from
running anything at all. Write build output under `$DIBS_SCRATCH`, never in `/tmp`. Nothing is
written on the machine to make a call, so a full scratch stops jobs, with exit 70, and never
`status`, `--peek` or `--gc`.

A job dies with the caller that started it: kill the caller, however you kill it, and the work
stops and the lock frees. A caller that stops answering, as a laptop does when it sleeps or a
shell does after Ctrl-Z, counts as gone after `DIBS_LEASE` seconds (120, and 0 turns it off).
`DIBS_NO_LIVE` and `DIBS_NO_WATCHDOG` each switch off half of that machinery, which is how it
gets debugged when it misbehaves. `DIBS_NO_CHILDREN` forces the fallback way of finding a holder's
descendants, which is otherwise unreachable on a kernel built the ordinary way.

An unreachable machine fails in seconds with a diagnosis, including whether Tailscale needs a
login, rather than hanging on a connection or a password prompt. Never retry that in a loop.

Every completed run's duration is recorded per label, and `--status` uses the median of those to
say how much longer the holder has and when each queued caller should start. It says nothing
rather than guessing when it has no history to go on.

Every job records which agent started it, taken from the title of its Claude Code session, which
is the name the user sees on the window it is running in. `--status` and `--log` carry it, so a
command on the machine leads back to the agent that can be asked about it. A shell that is not an
agent says so, and a session with no title yet is named by its id.

A holder is called idle when the CPU under it has not moved between two looks, which catches a
job that hangs after an hour of work as well as one that never started. That takes two looks,
and a `--watch` left running is what supplies them. `DIBS_IDLE_AFTER` sets how long a stretch of
nothing has to be, in seconds, before it is worth mentioning.

Never poll dibs. Launch it in the background and do other work: queueing then costs nothing, and
the notification arrives when the command is done.

## The two halves

`crates/dibs-runner/` is the half of dibs that runs on a machine: it takes the lock, runs the
job and reads the lock directory back for `status`. A call reaches it through a one-line `sh -c`
that ssh runs, and each client only ever talks to the runner built from its own source, which the
machine builds when it first lacks it, so nobody installs or upgrades anything there by hand.
`docs/design/protocol.md` is what passes between the two.

`crates/dibs/` is the other half, the `dibs` binary on your side. It reads every command line
through one grammar, chooses and reaches the machine, and is the recipe layer behind `dibs
build`, `test`, `bench`, `list`, `runs`, `gaps`, `shell`, `raw`, `batch`, `with` and
`machines`: a recipe's jobs are calls it makes itself, not commands it runs. The layer exists
because an interface taking one arbitrary string invites four problems that were measured in
the log it replaced. Labels were unstable, so estimates could not work. Two jobs in 179
redirected their output, so watching one almost never worked. Agents chose their own scratch
paths, and one filled a shared quota. And the rule to build under the shared lock was prose
rather than structure, so 17% of all exclusive time on the machine was spent compiling.

`crates/dibstop/pty-run.py` runs `dibstop` under a pseudo-terminal and prints what is on the
screen, which is how a change to it is checked. ratatui redraws by moving the cursor and
rewriting only what changed, so its output is no series of frames to split apart, and applying
the escapes to a grid is the honest way to read one:

    python3 crates/dibstop/pty-run.py "dibstop 2" ROWS COLS SECONDS [keys]

The keys are backslash-escaped and sent halfway through, so a wheel notch is `'\x1b[<65;10;5M'`
and a digit is just `5`.

## Related

The Claude hooks that stop an agent reaching the machine directly, or sleeping under the
exclusive lock are not here: a hook has to land in `~/.claude/hooks/` to do anything, which
makes it part of your own config rather than part of this. `dibs-agent-rules.md` describes
what they enforce, so you can write your own.
