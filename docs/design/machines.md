# Managing the machines

dibs knows how to reach a machine (the inventory) and what a machine has (`dibs --check`), but not
what each one should have. A machine set up by hand leaves no record of it, and the gap between
two machines shows up as a failed job: a tool missing on one, a person's key present on one machine
and not on the next, a repo clone that was never made.

This is the missing half: an expected state, a check against it, and a window that shows the
difference. The first two are built, as `dibs machines` and `dibs-machines`; the fixes and
offboarding below are not. It is not a provisioning system. A machine built by a playbook stays
built by it, and a machine set up by hand stays set up by hand; this reads both, and applies a small
set of safe fixes only where nothing else owns the machine.

## The expected state

A file of its own in each person's dibs configuration, never in this repository and not in a
shared recipes repository either: it names machines, people and keys, and some of the machines one
person reaches are nobody else's business. Not in the inventory itself, whose entries
`dibs --check --write` rewrites whole, which would erase anything written into one by hand. It is
`~/.config/dibs/fleet.toml`, and the guide has its format. A sketch:

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

- **Profiles** are what a machine needs:
  - `dibs`: cargo, which builds the runner; bash, which runs every job; rsync 3 and git.
  - `rust`: rustup, and the toolchains the repos pin, read from each repo's `rust-toolchain` file
    rather than listed by hand.
  - GPU stacks, with the driver or runtime version.
  - **Repos:** a clone at `~/prog/<repo>` of every repo whose recipes may run there, derived
    from the recipes.
  - `unprivileged`: the service account rules of `history/shared-machine.md`, no sudo and in no
    group that is root in all but name.
- **Keys** are matched by fingerprint, never by the comment field, which leaks emails.

## The check

`dibs machines [--json]`: one probe per machine in the inventory.
- **What it reads:** tool versions, rustup toolchains, driver versions, the fingerprints in
  `authorized_keys`, Tailscale SSH, clones and their remotes, `sudo -n true`, and groups.
- **What it gives:** the probe answers as `dibs-format`'s facts, which the client checks against
  the expected state, one finding per profile; `--json` prints the same report as JSON.
- **Transport:** the runner's check, the one `dibs --check` reads, which takes no lock, so a
  probe answers at once even beside a benchmark. What it runs there is small: `nvidia-smi`,
  `rustup` and a listing of the clones. A machine not yet in the inventory is not probed: `dibs
  --check` records it, and installs the runner that answers the probe.
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
- **Where the data comes from:** the survey behind `dibs machines`, through the `dibs` library.
  Probes run in the background and never block the window.

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

## Not built yet

- Fixes on machines set up by hand, and the playbook commands for the rest.
- Offboarding.

## Installing

The window is opt-in: `install.sh` builds it only when asked with a flag, since eframe is a real
build and most installs are on machines with no display. Once installed, `dibs --update`
rebuilds it along with the rest, and never adds it where it was not asked for.
