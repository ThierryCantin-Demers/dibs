# dibs

A lock over a shared benchmarking machine, and the interface agents use to reach it.

The problem it solves is small and specific: several people, and several agents each, want one
machine with a GPU in it. A benchmark is only worth reading if nothing else ran beside it, and
nothing in ssh says so. `dibs` makes that a lock, and `dibs-run` makes it an interface that
records what was actually measured.

    git clone https://github.com/ThierryCantin-Demers/dibs
    cd dibs && ./install.sh
    export DIBS_HOST=dibs@<machine>           # fish: set -Ux DIBS_HOST dibs@<machine>

Then `dibs-run list cubek`, or `bash dibs-onboard.sh` if you have never used the machine and want it
to prove itself first.

## Build caching

A machine with `sccache` installed gets `RUSTC_WRAPPER` set for every job, with the cache under
its scratch directory. It turns on by the tool being there rather than by a flag, so a machine
without it is unaffected, and `DIBS_NO_SCCACHE=1` turns it off.

Measured on the benchmarking machine: filling an empty target directory for the largest workspace here, whole
workspace takes 922s cold and 104s when sccache has seen the work before, at a 98.8% hit rate.
The cache is between a sixth and a quarter of the target directory it replaces, so it is small.

One consequence worth knowing: sccache runs the compiler inside its own daemon, which is
parented to init, so the compiling happens **outside the job's process tree**. The CPU figure
dibs reports for a holder counts that tree, so a build using every core can read as near zero.
Idleness is therefore judged on the job's output as well: a job whose log is still growing is
working, whatever its tree says. Without that, a healthy build reads as stalled and dibs tells
you to kill it.

Incremental compilation is turned off wherever sccache is on, and nowhere else. sccache
declines to cache an incremental build, so leaving it on means debug builds, which is most test
recipes, get nothing from the cache. The usual reason to keep incremental is fast iteration on
one tree, which is not what happens here: every job starts from a worktree at some commit.

Worktrees and target directories are both collected, on different clocks and for different
reasons. A worktree is per commit and disposable, so it goes after `DIBS_KEEP_DAYS` (14). A
target directory is one per repo, shared by every tree of it, and is the thing that makes a
build fast, so it goes only when a repo has stopped being built on that machine at all, after
`DIBS_TARGET_KEEP_DAYS` (45). A compilation cache is what makes even that reasonable: refilling
a collected directory costs a fraction of filling it the first time.

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
them. It works with `dibs` and with `dibs-run`, and `dibs-run --dry-run` prints which card it
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
cards or different machines, and nothing about the two numbers says so. The first benchmark
under a label records where it ran, and a later one elsewhere is refused, naming what it was
measured on before. `--new-series` moves a label deliberately and starts its history again,
rather than mixing the new numbers into the old, which would rebuild the thing the check exists
to prevent. Checked before the run and recorded after it, so a benchmark that failed claims
nothing.

`--check` also reports what each card is plugged into, walked to the root complex rather than
read off the endpoint: a card with its own bridge reports the width between its die and its own
upstream port, which says nothing about the riser above it. A card reaching the host over fewer
lanes than it can drive is worth knowing about before believing a number that moved data.

## What is here

| | |
|---|---|
| `bin/dibs` | the lock. One bash file, shipped over ssh, installs nothing on the far side. |
| `config/chips.toml` | what to assume about a chip when no runtime can probe it. |
| `dibs-run/` | the interface: verbs, recipes, labels, worktrees, provenance. |
| `dibs-tui/` | a live view of who holds the machines, one feed each. |
| `dibs-report/` | builds a single-page handoff report from the sources themselves. |
| `dibs-design/` | the plans, the settled decisions and their measurements, and what sharing a machine takes. |
| `dibs-agent-rules.md` | paste into `~/.claude/CLAUDE.md` so your agents know the rules. |
| `dibs-onboard.sh` | sets a new person up and demonstrates the lock actually blocking. |
| `tests/` | the lock protocol, on a scratch directory. Never touches a real machine. |

## The two halves

`bin/dibs` is the resource layer and stays bash because it travels over ssh: a machine needs
nothing installed to be usable, which is what makes adding one cheap.

`dibs-run` is everything above that, and runs on your side. It exists because an interface
taking one arbitrary string invites four problems that were measured in the log it replaced.
Labels were unstable, so estimates could not work. Two jobs in 179 redirected their output, so
watching one almost never worked. Agents chose their own scratch paths, and one filled a shared
quota. And the rule to build under the shared lock was prose rather than structure, so 17% of
all exclusive time on the machine was spent compiling.

## Tests

    bash tests/dibs-test.sh        # the protocol, locally, safe while others are working
    bash tests/dibs-live-test.sh   # needs a real machine
    cd dibs-run && cargo test

## Related

The Claude hooks that stop an agent reaching the machine directly, or sleeping under the
exclusive lock are not here: a hook has to land in `~/.claude/hooks/` to do anything, which
makes it part of your own config rather than part of this. `dibs-agent-rules.md` describes
what they enforce, so you can write your own.
