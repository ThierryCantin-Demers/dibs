# Sharing a machine with other people

What it takes for a benchmarking machine to serve several people, and why each piece is the way
it is. Written from doing it once; the failure modes below are ones that actually happened.

## One unprivileged account, not one account per person

Jobs run as a single service account. It has no sudo, is not in `wheel`, and **is not in the
docker group**.

That last one is the real point. The docker daemon runs as root and its group is
root-equivalent without a password: anyone in it can bind-mount `/` into a container. An agent
account in the docker group is an agent account with root. Running jobs as a human login was the
exposure being closed, and it is not closed by removing sudo alone.

Anything needing root belongs to a human, on their own account. A permission error from a job is
the design working; do not route around it.

The account's home is the entire blast radius of a mistake, which is the property you are
buying. One account also means one build cache serves everybody, which is worth more than it
sounds when a cold Rust build is minutes.

## Why one account rather than one per person

Every piece of lock state is keyed by uid. Two users on one machine take *different* locks, both
see the machine idle, and both benchmark at once. That is a silent wrong answer rather than a
missing feature, and it is why `--check` warns about a uid-keyed lock directory.

The alternative is a shared lock directory with a group and a setgid bit, which works and is
what `--check` tells you to set up. One account is simpler and gets the shared build cache for
free. Pick either, but pick one deliberately: the default is the broken case.

## Access

A tagged node on a private overlay network, rather than a port on the internet.

The arrangement that works: tag the machine, put the people in groups, grant `autogroup:self`
only, and use the policy's `users` field to restrict each side to the account it may land on.
Netmap trimming then means a guest's client sees *only* tagged benchmarking machines, so
personal devices are absent rather than merely unreachable.

Two things learned the hard way:

- **A typo in a group name saves cleanly and fails later.** Use the policy preview rather than
  trusting that it parsed. A misspelled owner address locks the owner out the moment the machine
  is tagged.
- **Tagging a device removes it from `autogroup:self`.** A rule that references only the tag
  will lock you out if your own group is wrong.
- **Tailscale SSH does not work over *shared* nodes.** Sharing a machine with someone is not
  enough; they have to be on the tailnet.

## Non-interactive PATH

A command arriving over ssh does not get a login shell, so profile edits and `~/.cargo/env` are
not sourced. Tools that live outside the default PATH need symlinks into a directory that is
always on it, rather than a profile change that only works when a human logs in.

This is the same boundary that makes environment knobs not reach the machine: ssh forwards
nothing, and anything that depends on the caller's environment has to travel explicitly.

## Onboarding

`dibs-onboard.sh` installs the tools, runs `--check`, and then demonstrates the lock by taking
it exclusively for 25 seconds while a second job queues behind it. The demonstration matters
more than the install: it is the only part that shows a new person what the lock does, and it
uses real work rather than a sleep, because a sleep under the exclusive lock stalls everyone on
the machine for its whole duration and that is the worst possible place to teach it.

`dibs-agent-rules.md` is the section to paste into an agent's instructions.
