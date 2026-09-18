# dibs

A lock over a shared benchmarking machine, and the interface agents use to reach it.

The problem it solves is small and specific: several people, and several agents each, want one
machine with a GPU in it. A benchmark is only worth reading if nothing else ran beside it, and
nothing in ssh says so. `dibs` makes that a lock, and its recipes make it an interface that
records what was actually measured.

## Setting it up

**What you need first.** An account on the machine you can already `ssh` into with a key, and
that is all: dibs installs nothing on the far side, ships itself over the connection, and needs
no root anywhere. One account shared by everyone is the intended shape rather than a compromise,
because it is what lets one build cache serve the whole team, and because a lock keyed to a uid
tells two people the machine is idle at the same time.

    git clone https://github.com/ThierryCantin-Demers/dibs
    cd dibs && ./install.sh

`install.sh` symlinks `bin/dibs` into `~/.local/bin` so a pull updates it, and builds the recipe
layer under `~/.local/libexec/dibs` and `dibstop` if cargo is present. Pass `--copy` if you would rather have files that do not move
under you. Without cargo you still have a working lock, just not the interface above it.

Then point it at your machine and record what is in it:

    export DIBS_HOST=you@machine               # fish: set -Ux DIBS_HOST you@machine
    export DIBS_ROOT=$HOME/prog                # where your checkouts live, for bare repo names
    dibs --check you@machine --write           # probes it, writes ~/.config/dibs/machines.toml

The entry is named by the machine's own short hostname rather than by the string you dialled,
so `--on` takes a name and not an ssh address. Run it once per machine, and read what it says
rather than only its exit code: it reports what the machine can do, and warns about the things
that are wrong in ways nothing else would tell you. There is no default host, on purpose, so
nothing goes somewhere you did not choose.

**The one thing the machine does need.** A recipe prepares a git worktree from a clone at
`~/prog/<repo>` on the machine itself, so each repo you want to build has to be cloned there
once. A machine without it is dropped from that repo's routing rather than sent work it cannot
do, and `dibs --check` lists what it has.

**Check it works, and see the lock actually block:**

    dibs --status                              # who holds it, who is queued
    bash dibs-onboard.sh                       # takes the lock, queues behind itself, explains

`dibs --help` is the full surface. `dibs-agent-rules.md` is the rules your agents follow, and is
the shortest useful description of how to use this well. Load it from your clone rather than
copying it, so a `dibs --update` updates the rules too: a line `@~/<clone>/dibs-agent-rules.md`
in `~/.claude/CLAUDE.md` imports it, and an `AGENTS.md`, which has no imports, can have it
included when that file is generated.

## Updating

    dibs --update

It fast-forwards the clone `dibs` was installed from, lists the commits that arrived, and reruns
`install.sh` when anything changed or when the installed recipe layer was built from another
commit. A `--copy` install has no clone to pull and says so.

## Sharing recipes with the people you work with

The local recipe layer, `~/.config/dibs/recipes`, can be a git clone. Keep it in a private
repository your team can reach, since recipes name your repos, and everyone clones it into place:

    git clone <your-recipes-repo> ~/.config/dibs/recipes

`dibs --update` pulls it along with `dibs` itself. An edit is an ordinary commit and push, and a
recipe still being tried out can sit uncommitted in the clone until it settles.

## A recipe that takes values

A recipe declares its knobs, so one procedure covers a sweep instead of a copy of itself per
value. The names are substituted into every command and into what each step exports:

```toml
[bench.reduce]
  [bench.reduce.params]
  backend = { choices = ["cuda", "vulkan", "cpu"], default = "cuda" }
  samples = { default = "10" }
  problems = { default = "sum_axis2,arg_topk,topk5_" }

  [[bench.reduce.step]]
  lock = "shared"
  run  = "cargo bench --no-run -p benchmarks --bench reduce --features cubecl/{backend}"

  [[bench.reduce.step]]
  lock = "exclusive"
  env  = { CUBEK_BENCH_SAMPLES = "{samples}", CUBEK_BENCH_PROBLEMS = "{problems}" }
  run  = "cargo bench -p benchmarks --bench reduce --features cubecl/{backend}"
```

    dibs bench cubek@local reduce --backend vulkan --samples 30

`dibs list <repo>` prints what each recipe takes, its default, and its choices where it has any.
A value outside the choices, a name the recipe does not declare, and a parameter with no default
left unset are all refused here, before anything is sent. Only declared names are substituted, so
`${VAR}`, `awk '{print $1}'` and the rest of the shell pass through untouched.

The label does not carry the values, deliberately: one label is one duration history, and a label
per value would predict none of them. The run record carries them, so two points stay
distinguishable in `dibs runs`.

A sweep is one submission:

    dibs bench cubek@local reduce --sweep samples=10,30,100 --reps 2

`--sweep` is repeatable and the combinations are the cross product; `--reps` runs each point that
many times. They become a batch of ordinary calls, one per point, run in sequence because they
share a worktree and its build cache, so you are woken once and get one summary with a row per
point. Every point is checked before any of them is queued.

The sweep has its own flag rather than splitting `--samples 10,30`, because a value may contain a
comma: `--problems sum_axis2,arg_topk` is one value, and `--<name>` always means exactly one.

`dibs shell` takes `--bench` for a one-off that is a measurement, and `--max <seconds>` where the
default cap is too short for it.

Two things in a recipe are refused when it loads, because both produce a number that looks fine:

- A step naming a relative `target/` path. `CARGO_TARGET_DIR` is redirected per tree, so that
  directory is not the one the build writes, and the step reads whatever an earlier tree left.
- A step that compiles under the exclusive lock, which holds the whole machine for work that
  tolerates neighbours. The message shows the two-step form: build with `--no-run` under the
  shared lock, measure under the exclusive one.

## A cache of its own each run

cubecl keeps autotune winners, compiled kernels and throughput numbers in one store, under
`target/environment` beside the outermost `Cargo.toml` above where a step runs, whatever
`CARGO_TARGET_DIR` says. Recipe steps run inside their tree, so each tree has its own. A fetched ref
gets a tree per commit, so comparing two commits is safe. An `@local` tree is named after the
checkout's path, so run, edit, run reuses it, and the second run reads the first one's winners and
throughput numbers.

    [bench.reduce]
    fresh = ["CUBECL_ENVIRONMENT"]

`fresh` names variables that get a value unique to each run, the same in every step of it, and the
record carries the value. `CUBECL_ENVIRONMENT` names a store rather than a path, so each run gets a
store of its own inside its tree, and it goes when the tree does. dibs gives a value, not a
directory, since a value is what such a knob takes. A new tree seeded from a sibling never takes
the sibling's `target/environment`.

A fresh store is cold, so every run pays for autotune. The benchmark's warmup has to absorb it or
the first measured iterations include it, and the spread across `--reps` now includes autotune
picking different kernels. A margin smaller than that spread is not a result. To keep compiled
kernels warm and turn off only the two caches that can fake a result, give the measured step
`env = { CUBECL_AUTOTUNE_CACHE = "0", CUBECL_THROUGHPUT_CACHE = "0" }` instead.

A repo whose cubecl config sets `path = "global"`, or a step run outside any cargo tree, keeps its
stores in `~/.cache/cubecl`, shared by the whole account, where a store per run is never removed.

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

## What was measured

Every recipe run appends a line to `~/.local/state/dibs/runs.jsonl`, or `$DIBS_RUNS`: the label, the
repo and the commit of every repo it built, the values, the machine and card, the procedure and
its fingerprint, the batch it was a step of, and for each step its lock, exit, seconds, job,
`built=` and log. A measured step first reads the machine's state into it (CPU governor, kernel,
NVIDIA driver), and the run ends `ok` or `failed`.

`dibs runs [label]` lists them newest first with the date, machine, label, the measured step's time
and lock, and the commits. A failed run is listed only with `--all`. Runs that differ in nothing
recorded are grouped with their median and range, which is the noise a difference has to beat,
and another governor or driver makes another group rather than a wider one. A label whose
procedure changed under the same values is called out with the step that changed.

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
dibs --on multigpu --hold --port api \
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
which it defines.

```toml
[service.gpu-servers]
build = "cargo build -p colony-gpu-server --features cuda,vulkan"
ports = ["cuda", "vulkan"]

[[service.gpu-servers.serve]]
name = "cuda"
run = "target/debug/colony-gpu-server --backend cuda --listen 0.0.0.0:$DIBS_PORT_CUDA"
ready = "tcp:cuda"
```

## More than one machine

`dibs --check <host> --write` records what it finds there as an entry in
`~/.config/dibs/machines.toml`, and `dibs --on <machine>` sends a call to one of them.

The inventory has two layers, the way recipes have three. Set `DIBS_REGISTRY` to
`user@host:path` and `dibs --registry-sync` fetches a shared machine list, cached locally and
refreshed on a clock rather than on every call. Your own file then holds additions and
overrides: a machine you name yourself wins outright over a shared entry of the same name, and
your own `default` beats the shared one, so where your work goes never needs anyone else to
agree. A registry that cannot be reached costs the freshness of a list and never the ability to
dispatch, because the cached copy stays. Without `DIBS_REGISTRY` there is no shared layer and
nothing changes.
`dibs --abi --all` says whether a binary built on one machine can run on another, as facts
rather than a hash: compatibility is directional, so it reports each pair each way.

`dibs --machines` says what is known, `dibs --forget <machine>` drops one and repoints the
default, and `dibs --status --all` shows every machine at once, which is how you find where a
job is actually running once work is being ranked. The inventory is not in this repo because it names your
hosts; only `config/chips.toml`, which is a statement about silicon, ships here.

With more than one machine, `dibs --any <command>` sends a shared job to the least busy one,
and `DIBS_ROUTE=1` makes that the default. `dibs --pick -v` shows the ranking without running
anything. Machines are ranked on `/proc/loadavg` rather than on what dibs itself holds, because
on a machine someone is working at, most of the competing load was never started through dibs.
A machine marked `workstation = true` is discounted further, since a build that takes every
thread costs whoever works there their editor. That is a property of the machine rather than of
whoever dispatched, so a headless box running the agents is not discounted for it.

**A repo's work goes where its build cache is, even when that machine is busy.** Each machine
reports which repos it has actually built, by a marker cargo writes rather than by the target
directory existing, since preparing a worktree creates that directory whether or not anything
is built in it. A recorded preference in `~/.local/state/dibs/affinity` breaks ties, and a
benchmark's claim is the one that sticks, because a benchmark is the run that cannot move.

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

What that turns into differs per runtime, and none of it is guessable:

`CUDA_VISIBLE_DEVICES` takes an index or a `GPU-<uuid>`, never a bus id. Handed one it does not
fail, it ignores the value and leaves every card visible, so a job looks pinned and is not. The
inventory's bus id is resolved to a UUID on the machine at launch, which also means a card that
has moved slots is followed rather than mistaken for its neighbour. A machine that cannot
answer for the card refuses the job instead of running it unpinned.

Vulkan needs two variables that fight each other. `DRI_PRIME` takes a PCI address and is the
only thing that tells two cards of one model apart, but it is Mesa's and does nothing for the
NVIDIA ICD. `MESA_VK_DEVICE_SELECT` is a layer above every ICD so it does reach NVIDIA, but it
keys on vendor and model, which names both halves of a matched pair. Set together the layer
reorders last and wins, which sends both aliases of a pair to one card while each looks pinned.
So `DRI_PRIME` always, and the model selector only where the model names one card.

Both Vulkan variables reorder rather than filter, unlike the CUDA one. The default device is
the one that was named, which is what almost all code asks for, but a job that enumerates and
picks an index itself can still reach another card.

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

## What is here

| | |
|---|---|
| `bin/dibs` | the lock. One bash file, shipped over ssh, installs nothing on the far side. |
| `config/chips.toml` | what to assume about a chip when no runtime can probe it. |
| `core/` | the recipe layer behind `dibs build`, `test` and `bench`: recipes, labels, worktrees, provenance. |
| `dibs-tui/` | a live view of who holds the machines, one feed each. |
| `dibs-report/` | builds a single-page handoff report from the sources themselves. |
| `dibs-design/` | the plans, the settled decisions and their measurements, and what sharing a machine takes. |
| `dibs-agent-rules.md` | the rules your agents follow, loaded from here into their instructions. |
| `dibs-onboard.sh` | sets a new person up and demonstrates the lock actually blocking. |
| `tests/` | the lock protocol, on a scratch directory. Never touches a real machine. |

## The two halves

`bin/dibs` is the resource layer and stays bash because it travels over ssh: a machine needs
nothing installed to be usable, which is what makes adding one cheap.

`core/` is everything above that, and runs on your side. It exists because an interface
taking one arbitrary string invites four problems that were measured in the log it replaced.
Labels were unstable, so estimates could not work. Two jobs in 179 redirected their output, so
watching one almost never worked. Agents chose their own scratch paths, and one filled a shared
quota. And the rule to build under the shared lock was prose rather than structure, so 17% of
all exclusive time on the machine was spent compiling.

## Tests

    bash tests/dibs-test.sh        # the protocol, locally, safe while others are working
    bash tests/dibs-live-test.sh   # needs a real machine
    cd core && cargo test

## Related

The Claude hooks that stop an agent reaching the machine directly, or sleeping under the
exclusive lock are not here: a hook has to land in `~/.claude/hooks/` to do anything, which
makes it part of your own config rather than part of this. `dibs-agent-rules.md` describes
what they enforce, so you can write your own.
