# Should this be rebuilt from first principles?

Asked because the design so far is shaped by what already existed, and because two things are
coming that were not considered when any of it was written: coworkers sharing one pool of
machines, and cloud instances instead of boxes under a desk.

The short answer: **not a rewrite, a split.** What is worth keeping and what is wrong are not
the same layer, and separating them is cheap now and expensive later. But the multi-user case
is more urgent than it looks, and the cloud case inverts an assumption the whole thing rests
on.

## Multi-user is not a future feature. It is a present silent failure

Every piece of state is keyed by uid:

```
DIR=${DIBS_LOCK_DIR:-${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/dibs-lock}
mkdir -p "$DIR" || { DIR=/tmp/dibs-lock-$(id -u); ...; }
HIST=${DIBS_HISTORY:-${XDG_STATE_HOME:-$HOME/.local/state}/dibs/history}
```

Two people on the same machine take **different locks in different directories**. Neither sees
the other. Both are told the machine is idle. Both benchmark at once, and both get numbers that
look fine.

This is not a missing feature to schedule; it is a wrong answer waiting for a second user. Every
user has to end up taking the same lock before anyone else is invited, whatever else is decided
here.

One thing already survives the transition: liveness is `[ -d /proc/$pid ]` rather than
`kill -0`, precisely because signalling another user's process fails with EPERM and would have
pruned live holders. That fix was made for a different reason and happens to be what a
multi-user lock needs.

### One unprivileged account, shared by everyone

Jobs run as a single `dibs` account that everyone uses. Onboarding a person is a line in
`authorized_keys`. Everything that was keyed by uid is then shared by construction, with nothing
to configure and nothing to get wrong: the lock, the scratch tree, the history, the log. The rest
follows cheaply, since `--kill` and `prune` stop caring about EPERM on another user's processes
and `/proc` inspection works uniformly.

**The account has no privileges.** No sudo, and not in `wheel` or `docker`. The docker group is
the exposure that actually mattered: the daemon runs as root, so anyone in that group can start a
container that mounts the host filesystem and walk out of it as root, with no password anywhere in
the path. Membership is root spelled differently. What is wanted is an account that can compile
and measure and do nothing else, and that is what this is.

**The boundary that matters is between humans and agents, not between colleagues.** The threat is
accident rather than malice: an `rm -rf` with a wrong variable in it, a worktree deleted while
someone else was measuring from it. People who can already ssh to the box are inside the trust
boundary anyway. The agents they run are the thing to keep away from the rest of the machine, and
an unprivileged account keeps them there.

**Its home is the blast radius, and that is accepted.** One confused agent can destroy everything
the account owns: the build trees, the caches, the history and the log. All of it is rebuildable
and none of it is anyone's source tree, so losing it is annoying rather than catastrophic, and it
is exactly what lets one build cache serve everybody.

**The build cache is what that buys.** 107 of 179 jobs in the log name a hand-written worktree
path, and several copy a `target/` directory from a sibling with `mv` or `cp -a` to avoid
recompiling. With per-person accounts, two people building the same repo keep two multi-gigabyte
target directories and neither can use the other's. One account lets dibs own one cache per repo
that everyone benefits from, which is a larger practical win than the locking.

**What is given up, honestly.** There is no isolation between the people sharing the account. File
permissions cannot stop one person's agent writing another's tree, because it is the same tree,
and nothing stops a confused `--kill` reaching another person's job. That was traded deliberately
for the one shared cache, on the reading that the people here trust each other and the accident
they are guarding against is their own.

**Attribution does not come from the uid.** dibs records the agent's identity for every job, which
is finer-grained than a person anyway, since it distinguishes two agents belonging to the same
one. Underneath that, Tailscale knows which node and which tailnet user a connection came from,
and sshd logs which key authenticated, so the audit trail survives as long as the entries in
`authorized_keys` carry meaningful comments. Both the agent identity and the Tailscale identity
have been verified working.

**Per-person accounts stay supported.** A shared group over real accounts, with a group-writable
lock directory, is what the code falls back to where there is no shared account, and it is what
`dibs --check` recommends there. That is the arrangement for a machine administered by someone
else, where an account cannot simply be created. Its lock directory is group-writable by
necessity, so a confused agent can delete someone else's holder *record*. That makes the queue
display wrong and nothing worse: the lock itself is an open descriptor held by a live process, so
the kernel's answer is unaffected and no two benchmarks can run at once because of it.

**Where a shared account would be wrong.** Untrusted users, or quotas that have to be enforced
rather than agreed. That is a different problem and it is the one Slurm is for.

**The tension with Slurm, which is real.** Slurm's fair share works on associations built from
users, so a single Unix account collapses everyone into one and fair share between people stops
working. Every other reason to adopt Slurm survives untouched: GRES for per-device allocation,
cgroup device constraint, `--exclusive`, prolog and epilog for clock pinning. If fair share
between people is ever wanted, it is either real accounts or fairness implemented here from the
identity dibs already records, and the second is finer-grained.

## What survives any rewrite, and what does not

The distinction that matters is **mechanism against topology**.

**Mechanisms, hard-won, and independent of who runs them or where:**

- the gate plus read-write lock protocol, with the gate released before the device lock
- liveness by `/proc` rather than by signal
- the CPU walk that counts reaped children through `cutime`/`cstime`, without which a working
  job reads as idle
- idle as a rate rather than a total, which is the only version that catches a job that hangs
  after an hour
- the liveness fifo and `pdeathsig`, so a job dies with the caller that asked for it
- a bootstrap line that fish, bash and dash parse identically
- percentile estimates, and the queue walk that does not serialize concurrent shared jobs
- reading a job's output by resolving its descriptors

Each of those is a bug that was found the hard way, and the comments in the script are where the
reasons live. A rewrite that discards them buys a clean tree and re-learns all of it.

**Topology, which is wrong for what is coming:**

- state keyed by uid, as above
- attribution by session title, which is a label rather than an identity
- `--kill` authorized by nothing: anyone who can reach the machine can stop anyone's job
- history and log as append-only files in one person's home directory
- shipping a bash payload per call, which is right for one trusted user and questionable as the
  interface to a shared service
- the assumption that machines are fixed, long-lived, and addressable

So "start from scratch" is the wrong frame. The mechanisms should be kept and the topology
replaced, which is a re-hosting rather than a rewrite, and the parallel-build-then-switch plan
is exactly right for it.

## The premise stays: scarce fixed hardware

Queue, wait, lock, release. That is the model, and cloud does not change it, because a cloud
machine is treated as **a machine**: long-lived, addressable, an entry in `machines.toml` with
an ssh alias like any other. Something else brings it up; dibs uses it and never provisions.

This is a deliberate narrowing and it is worth saying what it buys. Modelling elasticity would
mean the resource layer growing opinions about boot time, instance types, and cost, and it
would weaken the one property the measurements depend on: an instance that is created per run
is not the same physical card twice, so nothing measured on it can be compared against its own
history. Keeping the premise keeps the binding rule sound, with no special case for where the
machine happens to live.

One thing gets *more* important rather than less. A long-lived cloud machine can still be
rebuilt onto different underlying hardware without changing its name or address. The device
identity check at acquire, comparing the PCI ID and GPU UUID against what the binding recorded,
is what catches that, and it will correctly refuse rather than continue a regression series on
silicon that was quietly swapped.

## The shape this is heading for

Stated 2026-08-31, as the target rather than as a plan: many users, several agents each, on
workstations that sleep; one always-on machine holding the shared build cache and running the
agent sessions; several benchmarking machines, some always on and some not, with some of them
restricted to one person.

**Three separate wants get bundled into "a central orchestrator", and they have different
answers.** Taking them apart is most of the work.

**A shared registry of machines, which is simply required.** Every user writing out the same
machine list by hand does not survive contact with a workplace. This needs data distribution
rather than a daemon: one `machines.toml` on the always-on box, fetched and cached locally, with
the per-user file demoted to additions and overrides for personal machines. That is the layering
recipes already use, and a cached copy keeps working when the box is down.

**Built.** `DIBS_REGISTRY` names the source as `user@host:path`, fetched by scp because a
machine reachable at all is reachable that way, cached under `~/.cache`, refreshed on a clock so
that no ordinary call pays a round trip. The personal entry wins whole rather than key by key,
since half an entry from each file describes a machine that exists nowhere. A shared machine
cannot be forgotten locally and says so, because overriding it is a different act against a
different file.

**Jobs that outlive the laptop that started them.** A job is currently killed when its caller
dies, deliberately: `--pdeathsig` and a liveness fifo, because a job still running for a caller
that has gone is a job nobody is reading. Agents stay on their owners' laptops, which is where
people work and is not negotiable, so the caller is always something that sleeps. This is
therefore a real conflict rather than an inconvenience: a queue that outlives submitters and a
lock that dies with its holder cannot come from one mechanism.

**The resolution is that the always-on box owns the job, not the lock.** An agent submits and
disconnects; a runner there holds the job for its whole life, so the existing liveness machinery
keeps working unchanged and is simply watching a queue runner rather than an agent. The `flock`
stays on the benchmarking machine, where it has always been. Output already lands in
`$SCRATCH/out/<label>.log` and `--out` already reads it, so a detached job has somewhere to leave
results that its submitter can collect after the fact.

**Dispatch that is actually good, rather than several clients guessing from stale snapshots.**
Central wins here, and the objection to it is narrower than it first looks. A central dispatcher
is only a single point of failure if it is a mandatory hop. Built as an optimisation that
clients bypass when it is unreachable, it gives a global view while it is up and degraded direct
dispatch when it is not, which is better than Slurm manages: a dead `slurmctld` means nobody
submits anything at all.

**So the rule is not "no middle box", it is "nothing mandatory in the middle".** Everything the
central tier does should be something the pool can still do without it, worse. The registry
falls back to a cached copy, ranking falls back to polling, and the lock is `flock` in every
case because correctness never depended on any of it.

**The client-side dispatcher does not survive this unchanged, and that is the Slurm threshold.**
Propose-and-confirm is adequate for a few clients: correctness never depended on the ranking,
only on `flock`. Many users with many agents removes the evidence that deferred fair-share,
which was that nothing was being starved, and correlated decisions from stale snapshots get
worse with every client added. The central tier that answers this is Slurm's controller, and the
section below is what it would take over. The end state probably does have a middle tier; it is
just not one written here.

**Restricting a machine to one person is ssh, not dibs.** With a shared registry, "not listed
for anyone else" stops being the mechanism and becomes only the shared file not naming it, which
is why a personal machine belongs in the per-user layer rather than the shared one. The enforcement underneath it is `authorized_keys` on that machine,
because dibs ships itself over ssh: anyone who can log in as the account can take the lock,
whatever an inventory says. **ssh enforces, an inventory expresses intent.** A shared inventory
file would need an explicit field for this, and it would still be advisory.

This does sit against the one-shared-account recommendation in `shared-machine.md`. The two are
compatible, since `authorized_keys` holds whichever keys are put there, but access is then per
machine rather than per user within a machine.

## Slurm, and what it would actually take over

Many users, heterogeneous nodes, GPUs allocated exclusively or shared, fair share, accounting,
job queues. That is a cluster workload manager, and Slurm has done it for twenty years. Taking
it seriously as the backend is the right instinct, and a surprising amount of the multi-machine
design turns out to be Slurm configuration rather than code.

**What Slurm does better than anything written here would:**

- **Multi-user allocation**, which removes the uid-keyed lock problem at the root rather than
  patching it. Slurm owns who has what; there is no shared directory to get the permissions
  wrong on.
- **Per-GPU allocation** through GRES, `--gres=gpu:1`, which is the four-GPU box exactly. It
  sets `CUDA_VISIBLE_DEVICES` itself, and with cgroup device constraint the job **physically
  cannot see** a GPU it was not allocated. That is stronger than the environment variable
  pinning designed here, which a determined job can simply override.
- **Whole-node exclusivity** through `--exclusive`, which is the CPU-benchmark case, and it is
  the same mechanism rather than a second one.
- **Fair share**, deferred three times in these documents because nothing was starved with one
  user. With colleagues it stops being deferrable, and it is not a thing to write by hand.
- **Accounting**, which is a better version of the hand-rolled log.
- **Node features and constraints**, `--constraint=`, which is where `--needs` lands.
- **Job output to a file by default.** `dibs --out` exists because a job's output goes down the
  ssh channel and is kept nowhere, and it is nearly useless in practice because 2 of 179 jobs
  redirected. Under Slurm every job's output is already a file. The feature stops needing a
  trick.
- **Prolog and Epilog**, which run with the slurmd's privileges around each job on the node.
  Clock pinning needs root and a guaranteed reset even when a job is killed, and that is
  precisely what an epilog is. It removes the sudoers rule and the stale-pin problem together.

**What Slurm costs, plainly.** It ends the property that has made all of this cheap: nothing
installed on the target. slurmctld, slurmd on every node, munge with a shared secret and
synchronised clocks, and a configuration that must agree across machines. For one person and
two boxes that is absurd overhead for a problem that a shell script is currently solving. For a
team sharing ten, it is the obvious answer and writing a scheduler instead would be a mistake.

**What it does not do, and what dibs keeps either way:**

- **Comparability.** Slurm has no notion that two numbers should have come from the same
  physical card. Binding a label to a device, and checking at acquire that the device is still
  that device, stays here.
- **Capability truth.** Slurm's features are hand-configured, which is the curated-table problem
  again. The cubecl probe stays, and the natural integration is that it *generates* the node
  feature configuration rather than a person maintaining it twice.
- **Recipes and provenance**, the run record, the recipe hash, the resolved revisions.
- **The estimates**, since Slurm knows about time limits rather than about what a label usually
  takes.
- **The agent-facing verbs.**

**What it does not solve either, so the design still has to.** The build-then-measure
reservation is not free under Slurm: a dependent job still queues after the one it waits for.
Slurm's answer is priority and fair share rather than the gate-holding transition designed here.
Worth knowing before assuming the problem migrates away.

### What Slurm costs the machines

#### Perturbation while a benchmark is running

This is the question that matters, and "dibs leaves nothing resident when idle" does not answer
it. What matters is what the daemons do *during* a measurement.

Slurm's answer is `JobAcctGatherFrequency`, the interval at which slurmd samples a running job's
resource usage. **Default 30 seconds**, and Slurm's own documentation is candid that shorter
intervals cost more and that a value of `0` collects only at job termination, explicitly
"reducing Slurm interference with the job". So the perturbation is real and acknowledged rather
than hidden.

Measured against what is already happening here: `dibs --watch 1` costs **0.1ms of CPU per
tick**, 2ms of user and system time across 20 ticks. At one second it polls **thirty times more
often than slurmd's default accounting**, walking the holder's whole process tree each time. If
watching a running benchmark is affordable today, and the rules here treat it as affordable but
discouraged, slurmd's sampling is in the same class or lighter.

It is also tunable in a way the watch is not: `JobAcctGatherFrequency=0` on a benchmark
partition keeps end-of-job accounting through `sacct` and drops live sampling entirely. The
thing lost is `sstat` while a job runs, which is a fair trade for a partition whose whole
purpose is undisturbed measurement.

And one part of it cuts the other way. cgroup enforcement is kernel-side and free, and it makes
a neighbouring job **physically unable** to touch a GPU it was not allocated or to exceed its
memory. Today nothing is constrained and a misbehaving job can perturb a measurement freely.
That is a reduction in perturbation, not an addition.

**slurmctld does not go on a benchmarking machine.** The controller does scheduling and
database work on an unpredictable schedule driven by what everyone submits, which is a permanent
exception to the premise that nothing else runs, and unlike accounting it cannot be tuned to
zero.

**Decided: the laptop, while there is one user; a small always-on machine when there is not.**
That works, and it is worth being clear about what follows from it.

A controller outage does not stop work. Jobs already running keep running; what stops is
submitting and querying. For a single user whose laptop is closed, that is exactly the time
they were not submitting anything either, so the outage costs nothing. A sweep launched before
the lid closed runs to completion.

Two things this requires. `StateSaveLocation` must be on persistent local disk, because that is
what the controller recovers the queue from when it comes back. And the nodes must be able to
reach the controller at a stable address, which Tailscale already provides: the laptop has a
fixed tailnet address, so nothing needs to be rediscovered after a reconnect.

**The trigger for moving it** is a second person who needs to submit while the laptop is closed.
That is the moment it stops being a single-user convenience, and it is a move rather than a
redesign: slurmctld's location is configuration.

#### The operational cost, which is the larger one

**A node goes DOWN on its own and stays there.** Slurm marks a node DOWN when slurmd stops
responding for `SlurmdTimeout`, or when the node's configuration disagrees with `slurm.conf`.
By default (`ReturnToService=0`) it **stays DOWN until someone runs**
`scontrol update NodeName=x State=RESUME`. A wrong `RealMemory` in the config is a documented
common cause. On heterogeneous consumer hardware, mixed vendors and two distributions, config
drift is not hypothetical.

**munge needs a shared secret and clocks that agree.** NTP drift breaks authentication across
the cluster.

**The controller has to live somewhere always on, and every candidate is a machine being
benchmarked on.** Laptops are not always up, so slurmctld lands on the machine or on the four-GPU
box. Its load is small; the premise of exclusive locking is that nothing else runs, and this is
a permanent exception to that premise.

**The asymmetry that matters more than any of the above:**

| | today | with Slurm |
|---|---|---|
| what runs when nobody is using it | nothing | two daemons per node, plus a controller |
| how a failure presents | one command fails | a node leaves the pool |
| how a failure resolves | retry, and it works | someone notices and intervenes |
| what a failure costs while unattended | nothing accumulates | jobs queue against capacity that is gone |

Today's failures are **per-call and self-healing**. Slurm's are **per-node and sticky**: a node
marked DOWN persists while nobody is looking, and the thing that eventually reports it is a job
that never started. For a fleet maintained part time by one person, that investigation is the
real price, not the RAM.

**It is largely mitigable and should be if this is adopted.** `ReturnToService=2` returns a node
to service when slurmd registers with a valid configuration, which turns the common case from an
intervention into a restart. A node health check script catches a broken GPU before a job lands
on it. Neither is exotic, and both should be set up on day one rather than discovered.

**The threshold is the four-GPU machine, not the second user.**

The first version of this said: one user and two machines is a script, a team on a shared pool
is Slurm. That put the decision comfortably in the future, and it was the wrong reading of
where the cost actually is.

Look at what the four-GPU box requires under `flock`, all of it designed in
`multi-machine.md` and none of it written: per-device locks, the gate released before the device
is claimed so disjoint requests do not block, `CUDA_VISIBLE_DEVICES` pinning that a job can
override anyway, clock pinning that needs a sudoers rule and a reset path that survives
SIGKILL, and routing across machines.

Now look at what Slurm does with the same box: `--gres=gpu:1` with cgroup device constraint,
`--exclusive` for the whole machine, `--constraint=` for requirements, and an epilog that resets
clocks whatever killed the job. Not approximations of the designs above; better versions of
them, because the enforcement is in the kernel rather than in an environment variable.

So the crossover is not the second person. **It is the multi-device machine**, and that arrives
now. The work to make four GPUs safe under `flock` is most of the work Slurm would make
unnecessary.

**The option this opens:** the layer split allows different backends per machine. Slurm on the
four-GPU box, where its value is highest and where nothing currently depends on it, while the
bench1 stays on `flock` and keeps working untouched. That is a real trial rather than a
migration, and if Slurm turns out to be more operational burden than it is worth, the thing
thrown away is a configuration rather than a rewrite.

**What that costs:** running two backends at once, which is genuine complexity and is only
acceptable because it is bounded and temporary. It should end in one of them, deliberately.

### Preparing for Slurm without building it

Building a Slurm backend before Slurm is running would be writing against an interface nobody
has validated. But three things cost nothing now and make the eventual swap mechanical, and
they are worth doing whether or not it ever happens.

**Choose Slurm's vocabulary rather than inventing one.** `--needs` should express node features
and generic resources, because that is what it becomes: `--constraint=` and `--gres=gpu:1`. A
bespoke predicate language invented here would not translate, and would have to be thrown away.
Isolation is already the right shape: whole-machine is `--exclusive`, per-device is
`--gres=gpu:1`.

**Let the cubecl probe generate the node configuration.** Slurm's features are hand-maintained,
which is the curated-table problem wearing a different hat. The probe already knows the truth,
so it should emit the feature lines rather than a person keeping two lists in agreement.

**Do the layer split.** It is the whole of the preparation: an agent layer that owns recipes,
bindings, provenance and estimates, over a resource layer whose only job is to hand back a
device. Today that layer is flock over ssh. If it becomes `salloc`, nothing above it changes.

None of that touches the uid-keyed state at the top of this document, which is a wrong answer
waiting for a second user. The single shared account is what answers it, and it needs no daemon
and no scheduler.

## Therefore: split it in two, now

**The agent layer**: the spec, recipes, labels, bindings, provenance, output, estimates. This is
the part that is specific to measuring things and comparing the results honestly, and it is the
part worth owning.

**The resource layer**: "give me a device matching this requirement, and tell me when I have
it". One interface, several implementations:

- `flock` over ssh on a fixed machine, which is what exists
- Slurm, once the pool is shared enough to deserve it
- a broker of its own, only if Slurm turns out to be the wrong fit for a reason not yet known

A cloud machine is not a fourth entry. It is a machine, and it goes through whichever of these
manages the pool it belongs to.

Doing this split now costs a refactor and no new infrastructure. Not doing it means every one
of those futures is a rewrite. It also makes the parallel-build-and-switch plan tractable,
because what gets built in parallel is one implementation of a known interface rather than a
second copy of everything.

## What I do not know, and what would change this

- **What the existing cloud benchmarking work does.** There is apparently already work at the
  company on benchmarking from cloud machines. If it exists and has a provisioning path, dibs
  should be a client of it rather than growing its own, and that likely settles the provider
  interface. This should be found out before any of the cloud design here is trusted.
- **How many people, and whether they trust each other.** Fair share between four colleagues who
  can already ssh to the box is a politeness problem and needs attribution and a queue. Opening
  it wider is an authorization problem and needs something this design has none of.
- **Whether the fixed pool grows.** Two machines is a script. Ten is a cluster, and at ten the
  Slurm question stops being rhetorical.
