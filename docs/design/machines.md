# Managing the machines

dibs knows how to reach a machine (the inventory) and what a machine has (`dibs --check`), but not
what each one should have. A machine set up by hand leaves no record of it, and the gap between
two machines shows up as a failed job: a tool missing on one, a person's key present on one machine
and not on the next, a repo clone that was never made.

This is the missing half: an expected state, a check against it, and a window that shows the
difference. It is not a provisioning system. A machine built by a playbook stays built by it, and
a machine set up by hand stays set up by hand; this reads both, and applies a small set of safe
fixes only where nothing else owns the machine.

## The expected state

A file of its own in each person's dibs configuration, never in this repository and not in a
shared recipes repository either: it names machines, people and keys, and some of the machines one
person reaches are nobody else's business. Not in the inventory itself, whose entries
`dibs --check --write` rewrites whole, which would erase anything written into one by hand.
Machines several people share may later take their part from a shared registry, the way the
inventory already can. A sketch:

```toml
[person.alice]
keys = ["keys/alice.pub"]

[machine.box]
provisioned = { by = "ansible", source = "box-ansible" }   # or { by = "hand" }
people = ["alice", "bob"]
login = "keys"                        # or "tailscale": Tailscale SSH, and no key involved
paths = ["box.local", "box.example.ts.net"]
profiles = ["dibs", "rust", "cuda"]
```

Built in phase 1 as `~/.config/dibs/fleet.toml`; `docs/guide.md` has the format as it stands.

- **Profiles** are what a machine needs:
  - `dibs`: bash 5.1 or newer, flock, GNU timeout, rsync, git.
  - `rust`: rustup, and the toolchains the repos pin, read from each repo's `rust-toolchain` file
    rather than listed by hand.
  - GPU stacks, with the driver or runtime version.
  - **Repos:** a clone at `~/prog/<repo>` of every repo whose recipes may run there, derived
    from the recipes.
  - **The service account rules** of `shared-machine.md`: no sudo, not in the docker group.
- **Keys** are matched by fingerprint, never by the comment field, which leaks emails.

## The check

`dibs machines [--json]`: one probe per machine.
- **What it reads:** tool versions, rustup toolchains, the fingerprints in `authorized_keys`,
  driver versions, clones and their remotes, `sudo -n true`, groups, and disk free.
- **What it prints:** `key=value` lines, which the recipe layer diffs against the expected state.
- **Transport:** a machine in the pool is probed as a shared job, never a peek: even a cheap probe
  runs beside whatever is being measured, and the shared lock is what keeps it off a benchmark.
  A probe behind a running benchmark waits, and the window shows it queued rather than stale. A
  machine being set up, not yet in the pool, is probed over plain ssh, since nothing is measured
  there yet. The probe is written for POSIX sh, because it must run before the machine has what
  dibs needs.
- **Reachability:** every path is tested from here, by resolving the name and connecting to port
  22. A path that needs another network or tailnet is shown as declared and untested.
- **Unrecognised keys:** a key on a machine that belongs to nobody in the expected state is
  flagged.
- **Asleep or off:** a machine that does not answer is unreachable, not failing.

## The window

A desktop app rather than a dibstop screen: dibstop watches running jobs, as btop does, and this
manages what they run on.
- **Built on:** egui on eframe, kept on a version that a later move onto a toolkit built over egui
  can share, so that the move is a port of views, not a rewrite.
- **What it shows:**
  - machines by requirements;
  - machines by people, meaning whose key is where;
  - per machine: its paths, how it was provisioned, and each failure with its fix.
- **Where the data comes from:** `dibs machines --json`. Probes run in the background and never
  block the window.

## Fixes

- **A machine a playbook owns:** the fix is the playbook, shown as the command and tag to run and
  never applied here. Two sources of truth for one machine drift apart.
- **A machine set up by hand:**
  - dibs applies, as the service account: adding or removing a person's key, installing a
    toolchain, cloning a repo;
  - anything that needs an admin is shown as a command for a person to run: packages, Homebrew,
    power settings.
- **Offboarding:** remove a person's key from every machine, and list the tailnet shares to revoke
  by hand.

## Phases

1. The expected state and `dibs machines`, read-only.
2. The window.
3. Fixes on machines set up by hand, and the playbook commands for the rest.
4. Offboarding.

## Installing

The window is opt-in: `install.sh` builds it only when asked with a flag, since eframe is a real
build and most installs are on machines with no display. Once installed, `dibs --update`
rebuilds it along with the rest, and never adds it where it was not asked for.

## Open

- The expected state's file name.
