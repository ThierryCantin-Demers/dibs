# From one machine to many

`dibs` today serializes one machine. The question is what it becomes when there are several,
and when a machine is not one resource but five: a CPU and four GPUs that can be measured
independently but not freely.

This is the design, the reasoning behind each decision, and the order to build it in. Nothing
here is built yet.

## What does not change

Three properties are worth more than any feature added below, and every decision here is
constrained by them.

**Nothing is installed on the target.** The remote half travels on the command line each call.
A new machine is usable the moment ssh reaches it, which is what makes adding a second, third
and fourth machine cheap enough to bother.

**The lock dies with the process.** It is an open descriptor held by a live process, not a
record in a database that someone has to remember to delete. A machine that loses power comes
back unlocked. Nothing that follows may replace kernel arbitration with a userspace ticket
queue.

**The client is a shell script, the viewer is Rust.** A full rewrite was considered and
rejected for the remote half for the first reason above.

## The resource model

### Devices are locks, and the machine is a lock too

A machine is a tree, one level deep:

```
machine
├── cpu
├── gpu:0  a card
├── gpu:1  the same card again
├── gpu:2  a different one
└── gpu:3  an older one
```

Three ways to hold it:

| request | machine lock | device lock | who it excludes |
|---|---|---|---|
| shared: builds, tests, inspection | shared | none | only a machine-exclusive job |
| device-exclusive: one GPU, tolerates neighbours | shared | exclusive on one | others wanting that same device |
| machine-exclusive: CPU benchmarks, and any measurement that needs silence | exclusive | none needed | everyone |

The compatibility falls out of `flock` without a matrix: a machine-exclusive job cannot start
while any shared or device job holds the machine lock shared, and a device job cannot start
while a machine-exclusive job holds it exclusively. The CPU never needs a lock of its own,
because "I need the CPU to myself" is the same statement as "I need the machine to myself".

### The gate has to be released before the device is claimed

Today one FIFO gate orders every acquisition, which is what stops a benchmark being starved by
a stream of shared jobs. With four devices, that same gate becomes a bottleneck: a job queued
for `gpu:2` would sit in the gate and block a job that only wants `gpu:0`.

The fix is to take the machine-level decision under the gate and the device under nobody:

```
gate           →  machine lock (shared or exclusive)  →  release gate  →  device lock
```

A job waiting on a busy device is no longer holding the gate, so requests for disjoint devices
overtake each other freely. A machine-exclusive job still queues in the gate, so new device
jobs line up behind it rather than starving it, and it waits out the device jobs already
holding the machine lock shared. No new arbitration, same two flocks plus one.

### Isolation is a property of the request, not of the machine

This is the decision that determines whether any number the system produces can be trusted.

Four GPU benchmarks running at once on that box share one CPU, one memory controller, and one
set of PCIe lanes. A benchmark that measures a kernel with device-side events barely notices.
A benchmark that measures end-to-end throughput, or is host-bound, or moves a lot of data,
measures its neighbours as much as itself.

No global policy is right, because the answer differs per benchmark. So the request says:

    dibs --bench --needs gpu:nvidia --isolation device   # this job's GPU; others may use theirs
    dibs --bench --needs cpu        --isolation machine   # nothing else runs at all

**Decided: `machine` is the default and `device` is opted into.** The failure mode of the
wrong default is not a slow queue, it is a number that is wrong and looks fine, discovered
weeks later when a regression will not reproduce. Wasting the box is recoverable; quietly
contaminated history is not.

**Whatever is chosen, the isolation level is recorded in the duration history with the run.**
This is cheap and it is what makes the choice reversible: a number can always be traced to the
conditions it was measured under, and a label that has run both ways can be compared honestly.

## Naming the hardware

### Capabilities come from cubecl, not from a table here

The first version of this design had a hand-curated table, reasoning that detection means
parsing `nvidia-smi` and deriving capability from a version number, which gets this exact
hardware wrong:

| card | chip | compute capability | tensor cores |
|---|---|---|---|
| RTX 2060 SUPER | TU106 | 7.5 | **yes** |
| GTX 1660 SUPER | TU116 | 7.5 | **no** — replaced by dedicated FP16 units |
| RX 5700 XT ×2 | Navi 10, RDNA1, gfx1010 | — | **no** — WMMA arrives with RDNA3 |

Two cards reporting the same number and only one having tensor cores. The problem is real;
curating a table is the wrong answer to it, because **cubecl already detects this correctly and
the benchmarks are already written against what it reports.**

`cubecl_ir::HardwareProperties` carries `num_tensor_cores: Option<u32>` and
`min_tensor_cores_dim`, queried from the device. `Features` carries `matmul`, per-element-type
support with usages, `tma`, `plane`, `cube_cluster`, `copy_async`. That is the vocabulary a
benchmark's own capability checks are written in, so a requirement expressed the same way
cannot drift from the check the code performs. A table maintained here would be a second
opinion about the same silicon, and the wrong one to trust when the two disagree.

So: **`dibs --check` runs a small cubecl probe on each device and stores what it reports.**
Authoritative, because it is literally what the benchmark will see. Self-maintaining, because
new hardware needs no entry. Correct on the 1660 SUPER by construction rather than by someone
remembering.

`--needs` is then written in those same terms:

```
dibs bench cubek@ref gemm-roofline --needs 'gpu, num_tensor_cores >= 1'
```

**What this costs, stated plainly.** The probe is a binary, and dibs' best property is that it
installs nothing on the target. The compromise: it is built from the checkout already on the
machine, once per machine and toolchain, cached in that machine's scratch, and built under the
shared lock by `--check`. It is the one artifact that lives there, it is rebuildable from the
repo, and losing it costs a rebuild rather than an inventory.

**Two identities, answering different questions.** `DeviceIdentity` already draws this line and
it carries straight over:

- `fingerprint`, `ptx_sm90` or `hip-kernel_gfx1151`, is what the runtime compiles *for*: the
  **compatibility** key, will the same kernels run here at all.
- The PCI bus ID and GPU UUID are the **binding** key: is this the same physical card. Two
  identical 5700 XTs share a fingerprint and are not interchangeable for a measurement, because
  thermals, slot bandwidth and the silicon lottery are not.

`name` is display only. cubecl's own documentation says nothing may gate on it, and nothing
here will.

**Re-probe rather than trust the cache forever.** A driver upgrade can change what a device
reports, so the stored properties carry the driver version and the date they were taken, and
`--check` refreshes them. A benchmark whose device properties changed underneath it is a
comparison that quietly stopped being one.

### Two files, because they change at different rates

The inventory splits in two, and keeping them separate is most of the design.

**`chips.toml` is a fallback stub**, for a device no cubecl runtime can reach. If HIP does not
work on gfx1010 and no Vulkan backend is installed there, dibs still needs to know a GPU is
physically present, and `lspci -nn` plus one reviewed line gives it that much. It is
deliberately small: the probe is the source of truth for everything a probe can answer.

**`machines.toml` is what you own**, which cards are in which box and what is installed on
them. It changes when a machine does.

They live in different places, and that is not an accident of tidiness. `chips.toml` ships
in this repo under `config/`: it is a statement about silicon, true for everyone, and the half
worth sharing outright. `machines.toml` names your boxes and the ssh aliases that reach them,
so it goes to `~/.config/dibs/machines.toml`, beside the recipe overrides `dibs-run` already
reads from there. This repo is public; an inventory in it would be a list of someone's hosts.

```toml
# chips.toml — keyed by PCI vendor:device, which is what the hardware actually reports
[chip."10de:2705"]                  # verified by reading it off the machine
name   = "RTX 4070 Ti SUPER"
vendor = "nvidia"
arch   = "ada"
sm     = "8.9"
matrix = "tensor"
vram   = 16376

[chip."10de:1f06"]
name   = "RTX 2060 SUPER"
vendor = "nvidia"
arch   = "turing"
sm     = "7.5"
matrix = "tensor"

[chip."10de:21c4"]
name   = "GTX 1660 SUPER"
vendor = "nvidia"
arch   = "turing"
sm     = "7.5"                      # the same number as the 2060 SUPER above
matrix = false                      # and no tensor cores: TU116 replaced them with FP16 units
fp16   = "2x"

[chip."1002:731f"]
name   = "RX 5700 XT"
vendor = "amd"
arch   = "rdna1"
gfx    = "gfx1010"
matrix = false                      # WMMA arrives with RDNA3
```

Those entries are stubs, not claims: `matrix` here is what to assume when nothing can be
probed. Where a probe runs, its answer wins, and the entry is only identity and provenance.

```toml
# machines.toml
[machine.bench1]
ssh      = "bench1"                 # the alias as ssh config spells it
hostname = "bench1"                 # what `hostname -s` says, so it recognises itself
scratch  = "~/.cache/dibs"

  [[machine.bench1.device]]
  kind  = "cpu"
  name  = "AMD Ryzen 7 5700X"
  cores = 8                         # 16 threads

  [[machine.bench1.device]]
  kind     = "gpu"
  alias    = "gpu:4070ts"
  pci      = "0000:0a:00.0"
  chip     = "10de:2705"
  runtimes = ["cuda", "vulkan"]     # what is installed here, not what the chip could do
  clock    = 2400                   # pinned, from calibration; absent means unpinned
```

`runtimes` belongs to the machine rather than the chip on purpose. Whether ROCm supports
gfx1010 is a fact about the silicon and the driver; whether ROCm is *installed on this box* is
not, and routing has to care about the second. It is also where the open question about the
5700 XTs gets answered concretely: if HIP does not work there, those devices list
`["vulkan"]` and no HIP benchmark is ever routed to them.

### Nothing here is hand-written

Hand-writing PCI bus IDs is how an inventory goes quietly stale. `dibs --check <host>` reads
`nvidia-smi`, `rocm-smi`, `lspci -nn` and `lscpu`, and writes the machine entry itself. An
unknown `vendor:device` is a prompt to add one reviewed line to `chips.toml`, which is the only
step that needs a person, and it is the step that should need one.

The same command run later reports drift: a card swapped, a GPU that moved slots, a runtime
that disappeared. **The PCI ID of a bound device is also worth checking at acquire**, which
costs one `readlink`-grade lookup and catches the failure that binding exists to prevent: a
benchmark continuing its history on a card that was quietly replaced.

### Identity has to survive a reboot

`nvidia-smi` index order is not contractually stable, and on a mixed-vendor box the NVIDIA and
AMD tools each number from zero, so "gpu 1" is ambiguous on its face. The canonical identity is
the PCI bus ID, with the GPU UUID kept alongside for NVIDIA. Both are stable across reboots and
across driver reinstalls, and both can be handed to `CUDA_VISIBLE_DEVICES` directly, so the
runtime never has to trust an index we resolved earlier.

A short friendly alias (`gpu:2060s`) is what a person and a label use. The mapping lives in the
machine's inventory.

## What an agent asks for

```
dibs --bench --needs gpu:nvidia:tensor --label gemm-roofline <command>
dibs --bench --needs gpu:amd           --label gemm-roofline-rdna <command>
dibs --bench --needs cpu               --label cpu-fma-sweep <command>
dibs        <command>                                    # shared, any machine, as today
```

`--needs` is a requirement, not an address. Naming a machine stays possible (`--on bench1`) and
is the escape hatch, not the normal path.

## Routing

Everything in this section describes the `flock`-over-ssh backend. If the pool becomes a shared
team resource, most of it is replaced by Slurm configuration rather than reimplemented: GRES for
per-device allocation, `--exclusive` for the whole machine, constraints for `--needs`. See
`architecture.md` for that trade and the threshold. The layer above, recipes, bindings and
provenance, is unaffected either way, which is the point of keeping them separate.

**Recommendation for this backend: the dispatcher is the client, and there is no daemon.**

A daemon would buy global fairness and a single queue. It would cost the property that makes
this design cheap: a machine that is up is usable, with nothing running on it and nothing to
restart. It also introduces a component whose own death wedges every machine at once, which is
precisely the failure the lock-dies-with-the-process rule exists to prevent.

Instead the client resolves a request in three steps:

1. **Filter.** Which machines have a device meeting `--needs`, from the inventory.
2. **Propose.** Ask each candidate for its state, which is `--status --json` and already exists
   and is already cheap. Rank by what is free now, then by shortest estimated wait.
3. **Confirm.** Attempt a non-blocking acquire on the winner. Losing the race to another client
   is expected and is not an error: re-rank and try again. Only when nothing is free does it
   queue, on the candidate with the shortest estimated wait.

Correctness never depends on the ranking being right, because the acquire is still `flock`.
The ranking only decides where to wait, so a stale reading costs a suboptimal queue and never a
double-booked GPU.

### That ranking does not work for shared jobs

Ranking by "what is free now" assumes a request that can be refused. A shared job can acquire
on any machine not held exclusively, so every candidate answers free, the ranking collapses to
a fixed order, and every shared job lands on the same machine.

The confirm step cannot rescue it either, and the reason is worth stating because it is not
obvious: **propose-and-confirm self-corrects only where acquisition can fail.** Two clients
racing for one GPU produce a loser who re-ranks and goes elsewhere. Two clients starting shared
builds both succeed, so nothing tells either of them it chose badly. The corrective feedback is
structurally absent in exactly the case that needs balancing.

Three things follow.

**The load signal already exists and was built for something else.** The idle detector walks
each holder's process tree and computes CPU rates, to tell a wedged job from a working one.
That is occupancy. A sixteen-thread box running one `cargo build -j16` is not free for a second
build in any sense that matters, and `--status` already knows it. Shared jobs rank on that
rather than on freeness.

**Sample two and take the lighter, rather than poll every machine and take the minimum.**
Argmin over a full poll is what creates the herd: every client reads the same stale snapshot
and makes the same correlated choice. Choosing the less loaded of two random candidates is
cheaper and behaves far better under stale information, which is the permanent condition of a
dispatcher with no daemon and therefore no global view.

**A machine someone is sitting at is more expensive than its load says.** A build that takes
every thread costs nothing on a headless box and costs a person their editor on a laptop. If
the pool includes a workstation, ranking has to discount it beyond what CPU occupancy shows, or
the dispatcher competes with its owner for their own machine.

**Cache affinity is the counterweight, and it is not small.** Shared jobs are overwhelmingly
builds, and `dibs-run` gives each repo one build cache per machine. A warm
`$SCRATCH/target/<repo>` on one box is worth more than any imbalance that moving to another
would relieve, so spreading without weighing it makes the system slower while looking better
balanced. This is the strongest argument for sccache, which is on the open list as not
installed: it is what decouples placement from cache locality and makes balancing free to do.

**Where Slurm sits.** This is a better fit for it than per-device locking was. `slurmctld`
holds the global view and schedules against consumable CPU resources, so spreading concurrent
shared work is its core competence rather than an add-on, and it can hold a job and place it
when a node frees instead of choosing now or blocking. The price is the one this design keeps
refusing: a controller and a `slurmd` on every node, and a component whose death wedges every
machine at once. Load-aware ranking with a two-sample tie-break gets most of the benefit and
keeps a machine that is up usable with nothing running on it. Do that first, and let it be the
measurement that says whether the rest is worth a daemon.

## Keeping a benchmark on the hardware it was measured on

A benchmark rerun on different hardware produces a number that cannot be compared to its own
history, and the comparison is the entire reason for running it again. So the label becomes a
binding.

The first run of a label records `label → machine + device + isolation`. Every later run of
that label goes to that device and waits for it if it is busy. `--rebind` is the only way to
move it, and doing so starts a new history rather than continuing the old one, because it is a
new measurement series.

**If the bound machine is unreachable, the run fails.** It does not silently go elsewhere. A
missing number is a delay; a number measured on the wrong GPU and filed under the same label is
a wrong conclusion that outlives the outage.

That rule is about measurements, and it must not be generalised to shared work. A build has no
history to corrupt and no hardware it has to stay on, so a machine going away mid-compile
should send it elsewhere rather than fail it. The two cases differ because one produces a
number that will be compared to older numbers and the other produces object files. A laptop
offered to the pool for builds is what forces the distinction: it sleeps, it moves between
networks, and losing it is routine rather than an incident.

The binding lives on the machine that owns it, not in a central registry, for the same reason
the lock does: no state to synchronize, and a machine that comes back knows what it owns. A
client looking for a label asks its candidates in parallel and caches the answer.

This also settles a question the current estimator already has: **duration history keys on
label plus machine plus device plus isolation.** The same sweep on a 2060 Super and on a 5700
XT are different distributions, and merging them is what makes an estimate meaningless. The
percentile work already in place then reports per-device spreads, which is what it should have
been doing.

## Clock pinning

**A clock cannot be pinned for one command.** On both vendors it is a driver-level setting on
the device, it needs root, and there is no per-process scope: `nvidia-smi -i 2 -lgc 1500,1500`
changes that GPU for everything that touches it until `--reset-gpu-clocks` or a reboot. The
AMD equivalent writes `manual`, `profile_peak` or `perf_determinism` to
`power_dpm_force_performance_level` in sysfs, with the same reach.

**Exclusive ownership is what turns a global setting into a per-command one.** Holding the
device lock already means nothing else may touch that GPU, so pinning on acquire and resetting
on release gives exactly the per-command semantics the mechanism refuses to provide on its own.
This is the strongest argument for putting clock control in the wrapper rather than leaving it
to each benchmark: a benchmark that pins its own clocks is changing a device other jobs may be
using, and only the thing holding the lock knows that it is not.

Four things this has to get right.

**Root.** Both knobs need it. A sudoers rule narrowed to the exact `nvidia-smi` and sysfs
writes, not blanket sudo, and a machine without it degrades to running unpinned with a warning
rather than failing.

**A crash leaves the pin behind.** The lock is released by the kernel when the process dies,
and no trap runs on SIGKILL, so the reset can be skipped in exactly the case where something
went wrong. Two cheap defences, and both are needed: setting the clock at acquire is idempotent
and corrects a stale pin from the previous holder, and the existing prune, which already removes
records for processes that are gone, resets the clocks of any device with no live holder.

**Pin below the sustained ceiling, not at the boost ceiling.** A clock the card cannot hold
throttles, and throttling varies with ambient temperature and with what the other three GPUs
are doing, which reintroduces exactly the variance the pin was meant to remove, in a form that
is harder to see. The pinned value is per-device, found once by calibration, and belongs in the
inventory beside the capability table.

**Memory clocks are not always lockable.** `--lock-memory-clocks` is unsupported on a good
number of consumer parts. Treat a failure to pin the memory clock as a warning rather than an
error, and record in the run's history whether the pin actually took, since a number measured
unpinned should not be compared against pinned ones without knowing.

Worth doing regardless of anything else in this document: it removes the largest single source
of run-to-run variance, and it changes what `--isolation device` is worth, because a pinned
clock is far less sensitive to a neighbour's power draw. That makes it a prerequisite for
opting a benchmark down to device isolation with any confidence.

## Portability

**Shell.** The remote bootstrap is parsed by whatever login shell the target has, today fish on
one machine and bash on another. The existing constraint holds and should become an explicit
contract: the bootstrap line is a pipeline, single-quoted, with no `$`, no redirection, and no
process substitution, which fish, bash, zsh and dash all read identically. Everything past that
line runs under `bash` explicitly and may use anything.

**OS.** Assumed Linux on the *target*; `/proc` is load-bearing for the CPU walk, the child
walk, the liveness check and the output-reading feature, and there is no portable substitute
worth writing. Ubuntu Server and Fedora differ in nothing that matters here except which GPU
tools are installed.

**The client is a different question, and macOS is the case that matters**, because a coworker
on a Mac wants to send work to the Linux machines. Nothing on that path needs `/proc`: the
client resolves a machine, ranks the pool, and hands a script to ssh. Two things did break it
and both were incidental rather than structural. GNU `timeout` is absent on macOS and is
`gtimeout` where coreutils is installed, so the poll bounds go through a shim that degrades to
running unbounded rather than to not running. And `${x,,}` needs bash 4 where macOS ships 3.2,
so the one use is a `tr`. Neither cost anything; the point is that being a client and being a
target are different requirements and only the second one needs Linux.

**A Mac is not a build node, and containers do not make it one.** Cross-building Linux binaries
on Apple Silicon means emulating x86-64 or maintaining a cross toolchain, and neither has a CUDA
story at all, which is most of what gets built here. The compat probe is unambiguous about the
artifacts: `Darwin` and `Linux` do not load each other'"'"'s binaries whatever the CPU underneath,
so nothing built there can be measured here. What a Mac can do is dispatch, and that is worth
supporting properly rather than approximately.

**Onboarding.** Adding a machine should be one command that proves it will work rather than a
checklist someone follows: `dibs --check <host>` verifies ssh, `bash`, `flock`, `timeout`,
`setpriv`, `/proc/<pid>/task/*/children`, a writable scratch directory off tmpfs, the
shell-agnostic bootstrap, and reports the GPUs it found with the capabilities the table gives
them. It is the difference between a second machine taking ten minutes and taking an afternoon.

## Order of work

Each phase is useful on its own and none requires the next.

1. **Address a machine.** `--on <machine>`, and the inventory file that names them. `HOST` and
   `TARGET` are already environment overrides, so this is close to free and immediately makes
   the new box usable exactly as the machine is today. **Done.**
2. **`dibs --check`.** Onboarding, and the thing that makes phases 3 onward safe to develop
   against a machine that is still being built.
3. **Inventory and the capability table.** Detection identifies the chip; the table says what
   it can do. No behaviour change yet, but everything after needs it. **Detection is done**:
   `--check --write` records what it found, keyed by PCI `vendor:device`, and replaces a
   machine's whole entry so a removed card disappears rather than lingering. The cubecl probe
   is the half still missing, and it is worth doing when there is hardware it can tell apart:
   today one NVIDIA card is the whole of the GPU inventory, and `chips.toml` already covers
   naming it.
4. **Device locks and isolation levels.** The gate change, the per-device locks,
   `CUDA_VISIBLE_DEVICES` pinning so a job physically cannot touch a GPU it does not hold, and
   isolation recorded in history. This is the phase that makes the four-GPU box worth having.
5. **Routing.** `--needs`, filter, propose, confirm. **Shared-job routing is done**, opted
   into with `--any` or `DIBS_ROUTE=1`: candidates are polled in parallel, ranked on
   `/proc/loadavg` over core count with the dispatching machine discounted, ties broken at
   random, and a machine that does not answer is named rather than dropped. `dibs --pick`
   exposes the choice, which is how `dibs-run` pins every step of a run to one machine.
   `--needs` filtering is the half still missing.
6. **Bindings.** Label pinned to device, `--rebind`, history keyed per device.
7. **The viewer.** `dibs-tui` grows a machine column and a device column. **The machine half
   is done**: one feed per machine, per-machine state in the header so that "the feed is down"
   and "it is idle" stay different answers, and `--kill`, `--out` and `--peek` routed back to
   the machine the selected row is on. The column hides itself when there is one machine, so
   nothing changes for someone without an inventory. It was left last on purpose and then done
   early, because routing made a single-machine view actively misleading rather than merely
   incomplete. The device column waits on phase 4.

Phases 1 and 2 are worth doing before the machine is finished being built.

## Where this stands

The agent-interface plan beside this one is essentially complete: labels, output, scratch,
worktrees, caches, the structural build/measure split, recipes with steps, provenance and its
reader, and the instrumented escape hatch. All of it is verified against a real machine.

Phases 1 and 2 are done, and phase 3 is done apart from the cubecl probe. What unblocked
them was giving up on waiting for a second benchmarking machine and putting the laptop in the
inventory instead. It cannot measure anything, and that turns out not to matter: everything in
this plan below the lock is dispatch, and a laptop exercises three cases an always-on desktop
cannot. It is a different vendor, so `--needs gpu:nvidia` is a real filter rather than a
predicate that always passes. It sleeps, so the rule that a run bound to an unreachable
machine must fail rather than reroute is exercised constantly instead of never. And it is
localhost, so routing has to handle a candidate that locks locally without ssh, which the
multi-GPU box will also be whenever someone works on it directly.

The laptop is where `measure = false` comes from. A machine that cannot produce a trustworthy
number should be unable to produce one at all, rather than every agent remembering not to ask
it, and the flag is set from a detected battery rather than from a missing GPU: a CPU
benchmark box has no GPU and is still worth measuring on.

Shared-job routing landed next, out of phase order, because it is the one piece whose value
arrives with a laptop in the pool rather than with the four-GPU box. It stops short of `--needs`
and deliberately refuses to route benchmarks at all: a measurement's history and its label
binding key on the machine it ran on, and phase 6 is what would make moving one safe.

Phases 4 through 7 remain blocked on real hardware, but less completely than before. Phase 4's
protocol is `flock` logic and can be developed against a synthetic inventory in the test suite;
what needs the four-GPU box is proving its value, which is disjoint devices on one machine
overtaking each other, and a laptop with one iGPU cannot show that. The load-bearing open
question is still the one below: whether whole-machine or per-device isolation is the right
default.

**Slurm is not a phase, it is a fork at phase 4.** Phases 1 to 3 happen either way, and the
capability probe would generate Slurm's node features rather than feed a dispatcher of our own.
Phases 4 and 5, device locks and routing, are exactly what GRES with cgroup device constraint,
`--exclusive` and `--constraint=` replace, and replace better: a cgroup makes a job physically
unable to see a GPU it was not allocated, where environment-variable pinning is only advisory.
Phases 6 and 7 stay regardless, because Slurm has no notion that two numbers should come from
the same physical card. The decision point is when a multi-GPU box exists, and the sane trial is
Slurm on that box alone while everything else stays on flock, which the layer split in
`architecture.md` makes bounded.

## Decisions that are still open

- **Whether CPU benchmarks belong on that box at all.** Four GPUs idling still draw power and
  move air in a shared thermal envelope, and their power states affect CPU boost behaviour.
  Machine-exclusive locking removes the software contention and not the physics.
- **Whether ROCm usefully supports gfx1010 at all.** RDNA1 has never been on AMD's officially
  supported list and the situation has moved around over the years. If it does not, those two
  cards are reachable through wgpu and Vulkan rather than HIP, which is fine for a good deal of
  what they would be used for but is worth knowing before the design assumes a ROCm path.
  Verify on the machine rather than from documentation.
- **Where clock calibration lives.** The pinned value per device has to come from somewhere,
  and a calibration run that finds the highest sustainable clock is itself a benchmark that
  needs the machine to itself.
- **Fair share across agents.** Still nothing is starved, only delayed, and this remains
  deferred. Several machines make it less pressing rather than more.
