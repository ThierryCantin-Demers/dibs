# Build images: a binary that runs where it was not built

Status: built: shared jobs, recipes, `--image` and the dibspatcher.

## The problem

A split builds on one machine and runs on another. The first real split built against glibc 2.44
and staged on a machine with glibc 2.39, and the stage check refused it: the binary asked for
eight libm functions (`acosf`, `acoshf`, `asinf`, `atan2f`, `coshf`, `sinhf` at `GLIBC_2.43`,
`cosh` and `sinh` at `GLIBC_2.44`) that 2.39 does not have. The build profile pins the CPU
baseline (`x86-64-v2`) but nothing about the C side, so any build machine newer than a GPU machine
breaks every split between them.

The fix cannot rewrite the command. dibs runs arbitrary commands, and a recipe's build is whatever
its author wrote: swapping `cargo build` for a cross-linking wrapper, or adding linker flags,
works for one recipe and silently does nothing for the next. The command has to run unchanged,
against an older C library.

## The idea

A build runs inside an **image**: the root filesystem of an older distribution, pinned by digest,
which supplies the C library, the C toolchain and the system libraries the binary links against.
The command is the same; only the system it sees changes. The measurement still runs natively on
the GPU machine, so timing is untouched, and the stage check still verifies the binary links there.

The image carries only the C side. The Rust toolchains come from the account's own `RUSTUP_HOME`,
mounted read-only: rustup's official toolchains need only glibc 2.17, so they run in any image,
`rust-toolchain.toml` works as it does now, and a Rust upgrade never needs a new image.

One image serves every machine at or above its glibc. glibc is backward compatible through symbol
versions, so a binary linked against 2.39 runs on 2.39 and anything newer. The floor is the oldest
glibc among the machines the binaries run on, and moves down only when an older one joins.

## What the binary loses

At run time the binary loads the running machine's glibc, so `memcpy`, `malloc` and the rest are
that machine's implementations, chosen for its CPU when the program loads. What the floor fixes is
which version of each symbol the binary asks for. A function glibc has since re-versioned, like the
eight above, is called through its older entry point on a newer machine: possibly a compatibility
wrapper, and possibly a last-bit difference where the implementation was replaced. C and C++ that
crates compile in their build scripts use the image's compiler, which is older than a current
distribution's. Rust code is unaffected: rustc generates it, and its standard library is already
built against glibc 2.17.

In exchange the C side no longer drifts with the build machine's updates. Moving a series to an
image is one new baseline; after it, upgrading a build machine no longer moves the numbers.

## Caches

dibs prepares every cache on the host before the command and checks it after, and the image sees
each at **the same path** as the host, so `CARGO_TARGET_DIR`, `$DIBS_SCRATCH` and `$TMPDIR` mean
the same inside and out, and cargo's fingerprints, which hash paths, stay valid.

- **Target directories.** A fetched commit builds in one target per repo,
  `target/<repo>[-<nest>][-arm<n>]`; a sent tree in `target/<repo>-local-<key>`. An imaged build
  adds the image's digest to that name. This is required, not an optimisation: with the toolchain
  mounted from the host, rustc is the same inside and out, so native and imaged builds would share
  fingerprints while their build scripts and C objects link different C libraries.
- **Seeding.** A new target is reflinked from the sibling that has built the most of its lockfile,
  and siblings' saved copies are merged into it. Both choose only among targets of the same image.
- **The tree claim.** `.dibs-tree`, written before a build, is what makes a later measurement exit
  78 when another tree built there since. It needs nothing new, being host-side.
- **The building mark.** A build holds a lock on `.dibs-building.*` through a descriptor its job
  inherits, which tells recovery whether a compiler survived. The sandbox must pass it through.
- **`built=`** is counted from cargo's `Compiling` and `Finished` lines in the log, so it works
  unchanged.
- **CARGO_HOME** is mounted whole, not only its registry: cargo's download lock lives at its root,
  and dibs reads that lock before freezing a build, so a build frozen mid-download never holds
  every other build's downloads.

## Running the command

dibs keeps no cgroups. It stops a job by signalling its process group, freezes a preemptible build
with `SIGSTOP` to that group while a benchmark runs, parks low-priority work by killing the group,
detects idleness by summing CPU over the job's descendants in `/proc`, and sweeps processes that
left the group by `DIBS_JOB` in their environment. A sandbox has to keep the build inside all of
that.

**bubblewrap does.** `bwrap` execs the command as a descendant of the job, in its process group,
with the descriptors and environment it was given:

```
bwrap --ro-bind <rootfs> / --dev /dev --unshare-pid --proc /proc --tmpfs /tmp \
  --ro-bind /etc/resolv.conf /etc/resolv.conf --ro-bind /etc/hosts /etc/hosts \
  --bind <home> <home> [--bind <scratch> <scratch>] -- bash -c '<command>'
```

- The account's home and the machine's scratch are bound whole, at their own paths. Everything
  dibs keeps lives under them: worktrees, targets, `TMPDIR`, `CARGO_HOME`, `RUSTUP_HOME` and the
  clones fetched trees are worktrees of. Binding the two leaves nothing to forget.
- `--unshare-pid` is what lets an unprivileged `bwrap` mount `/proc` at all. The host still sees
  every process inside, so idle detection and the escape sweep read them as now.
- Mount points are made in the unpacked root before each run. A link the image holds on the way
  to one is refused rather than followed, since it would be followed on the host; a link at
  `/etc/resolv.conf`, as systemd-resolved leaves, is replaced by a file to bind over.
- The root is read-only, with a private `/tmp`, and `$DIBS_SCRATCH/images` is bound read-only over
  the scratch bind, so one build cannot change the copy of an image every build shares. Mount
  points for home and scratch are made when the image is unpacked.
- A directory of scratch that is a link to another disk is bound at its real path too, so the
  link resolves inside.
- `resolv.conf`, `hosts`, the account added to the image's `passwd` and `group` when it is
  unpacked, and the image's `ca-certificates` are what cargo needs to fetch, over https or ssh.
- `bash -c`, as every job runs now: an image's `sh` may be dash.
- `--die-with-parent` is not needed: the tether already stops the group when the runner dies.
- A `--hold` job runs on the host: its command only waits on a fifo in the lock directory.

**podman does not.** `podman run` hands the container to `conmon`, which is no descendant of the
job; it cannot pass `SIGSTOP` or `SIGKILL` on, so a frozen split build would go on compiling beside
a measurement; and it does not pass inherited descriptors without `--preserve-fds`. Each of those
would need code of its own in every path that stops, freezes or parks a job.

## The environment inside

The job's environment is built on the host, and bubblewrap passes it as it is, so an image's own
`ENV` settings do not apply. Two things follow:

- `PATH` puts `~/.cargo/bin` first. rustup's proxies there run in any image; but any other tool
  installed there was built against the host's glibc and fails to load inside.
- Host-wide cargo configuration (`~/.cargo/config.toml`) applies too, so a `RUSTC_WRAPPER` such as
  sccache or a linker such as mold set there would be the host's binary, and fail. An imaged build
  needs a cargo configuration of its own for those, or none.

## Getting images

An image is named `<ref>@sha256:<digest>`; a tag alone is refused, since only a digest pins it.
It is fetched with `skopeo copy` and unpacked with `umoci unpack --rootless` into
`$DIBS_SCRATCH/images/<digest>/`, once per machine, under a lock of its own, by the first job
that needs it, within that job's cap and stopped with it. That
needs no `/etc/subuid` range, which an admin would have to grant, and keeps images where scratch
accounting sees them; podman's own store, under `~/.local/share/containers`, would be invisible to
it. Each use touches the image's `.dibs-used`, and a job holds a shared lock on the image for as
long as it runs. `dibs --gc`, and the sweep every prepare runs, list images by last use and remove
one unused for the general keep, made writable first, only once its unpack lock and that use lock
are both free; an unpack whose process is gone goes too. `dibs --check` says whether bwrap is on a
job's PATH, without which no image runs, and skopeo and umoci, without which none is fetched.

The image can be a public one: `buildpack-deps:noble` is Ubuntu 24.04, glibc 2.39, with gcc,
`pkg-config`, git, `ca-certificates` and the common `-dev` libraries. One with more is a Dockerfile
on top of it, built once, published, and referred to by digest.

## Naming an image

`DIBS_IMAGE=<ref>@sha256:<digest>` in a call's environment names it, as `DIBS_ATTEMPT` names an
attempt, so every job a recipe or a batch makes carries it. `--image <ref>@sha256:<digest>` on a
call or a recipe sets it, and a recipe's `image = "..."` does when neither the flag nor the
variable chose one, the flag winning over both. A recipe's image without a digest is refused when
it loads. A shared call runs in the image; a
measurement runs natively what was built there, since the image has neither the machine's GPU
devices nor its driver, and a tree laid out by a measurement still gets the image's target. The
transfer of a sent tree carries it, since that is where the tree's target is chosen, and so does a
detached batch's driver. A name without a digest is refused for every mode but a sync that lays
out no tree, which copies files and ignores the image. A shared call with no tree dibs lays out is
refused too, since its cargo would build in whatever target the command names, natively built
objects included. A recipe's record carries the image, and its fingerprint differs
from the same recipe built natively.

The dibspatcher's profile names one image for every split build, beside its `rustflags`, and
the bundle's attested inputs include it; a native build attests what it did before. The `rustc -vV` recorded with a bundle stays right, since
the toolchain is the host's.

## The stage check

The floor is more than glibc: `libstdc++` and the libraries `-sys` crates link, such as `libssl`,
come from the image's distribution too. The stage check runs `ldd` on the GPU machine and refuses
a binary with a library or version not found there, which covers both. It also refuses when
`ldd` itself fails, unless it says the binary is static. Nothing checks what a binary opens with
`dlopen`.

## Not covered

- Measuring inside an image. The GPU machine runs the binary natively.
- macOS runners: images are Linux only.

## Open questions

1. A machine-wide default image, or only a recipe and a profile naming one.
2. How an imaged build's cargo configuration is given, when the host's names tools that cannot run
   inside.
