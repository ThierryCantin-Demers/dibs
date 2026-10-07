# dibs

A lock for shared GPU benchmarking machines, and the command line AI agents use to reach them.

Several people, each running several agents, share a few machines with GPUs in them. A benchmark
only means something if nothing else ran beside it, and nothing in ssh says whether anything did.
dibs makes that a lock: builds and tests share a machine, a measurement gets it to itself, and
everyone else queues. Above the lock, recipes turn "build this repo at this commit and benchmark
it" into one command that records what was actually measured.

## Read this first

- **It is built for agents first.** Claude Code and Codex sessions are its main users, and its
  output, its refusals and its rules are shaped for them: terse trailers, exit codes with precise
  meanings, one summary per batch. A person can use it directly, but much of it is awkward by hand.
- **It is built for the Burn ecosystem first.** Burn, CubeCL, CubeK and the projects around them
  are what it was designed and tested against. The lock runs any command, but the layer above it
  assumes Rust workspaces built with cargo. Other projects may not work well without changes.
- **The code was written by Claude, not by the maintainer.** All of it was written by Claude,
  Anthropic's AI model, in Claude Code sessions the maintainer directed. It has a test suite and is
  used every day, but do not take it as given that it is good code or that everything works.

## What it does

- `dibs <command>` runs a command on the machine under a shared lock, several at once.
- `dibs --bench <command>` runs it under an exclusive lock: nothing else runs meanwhile.
- `dibs status` says who holds each machine, who is queued, and roughly for how long.
- `dibs bench <repo>@<ref> <recipe>` builds a repo at a commit in a tree of its own, measures it,
  and records the commits, the values, the machine and the card.
- `dibs batch` takes a list of calls as one submission and prints one summary at the end.
- `dibstop` is a live view of every machine, for people.

## Setting it up

You need an account on each machine that you can already `ssh` into with a key. Nothing is
installed on the machine and no root is needed. One account shared by the whole team is the
intended setup, since it lets one build cache serve everyone.

    git clone https://github.com/ThierryCantin-Demers/dibs
    cd dibs && ./install.sh

`install.sh` builds `dibs` and `dibstop` with cargo and installs them in `~/.local/bin`, or
`$PREFIX/bin` when `PREFIX` is set, and with `--machines` the `dibs-machines` window too. The
part of dibs it sends to the machines is built into the binary, so an edit in the clone changes
nothing until the next install, and only `dibs --update` needs the clone. Installed over a dibs
from before, it replaces it, the bash script included.

Record each machine, and say where your checkouts live:

    dibs --check you@machine --write    # probes it, writes ~/.config/dibs/machines.toml
    export DIBS_ROOT=$HOME/prog         # fish: set -Ux DIBS_ROOT $HOME/prog

Read what `--check` prints, not only its exit code. With one machine recorded, every call goes
there. With several there is no default: a call names its machine with `--on`, shared work that
names none is placed on one, and a benchmark that names none is refused. Recipes build from a
clone at `~/prog/<repo>` on the machine, so clone each repo you want to build there once. Then
see who holds each machine:

    dibs status                         # once
    dibstop                             # live, and a way to act on what is holding it

`dibs --update` pulls the clone and your recipes, and reinstalls when something changed.

## Recipes

dibs ships no recipes. You describe a repo's builds and benchmarks in
`~/.config/dibs/recipes/<repo>.toml`, which can be a git clone your team shares:

```toml
[bench.solve]
  [[bench.solve.step]]
  lock = "shared"
  run  = "cargo bench --no-run --bench solve"

  [[bench.solve.step]]
  lock = "exclusive"
  run  = "cargo bench --bench solve"
```

    dibs bench app@local solve                   # your working tree, uncommitted changes included
    dibs bench app@main..local solve --reps 3    # your branch against where it left main
    dibs runs                                    # what was measured, and what compares

## Giving it to your agents

Load [`dibs-agent-rules.md`](dibs-agent-rules.md) into your agents' instructions from your clone,
so an update updates the rules too. In Claude Code, a line `@~/<clone>/dibs-agent-rules.md` in
`~/.claude/CLAUDE.md` does it.

The rules tell an agent never to ssh a machine; `dibs hook ssh` makes it so. It is a Claude Code
PreToolUse hook: it reads the tool call on stdin and refuses, with exit 2 and the dibs call to
use instead, an ssh, scp, sftp or rsync aimed at any name a machine in your inventory goes by. An
rsync through dibs's own transport passes. To wire it, add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [{ "type": "command", "command": "dibs hook ssh" }] }
    ]
  }
}
```

A hook that stops an agent sleeping under the lock is still yours to write.

## Learn more

- [`docs/guide.md`](docs/guide.md): every feature in detail, and why it behaves the way it does.
- `dibs --help`: every flag.
- [`docs/design/`](docs/design/README.md): how it is built, and the decisions and measurements
  behind it.

## What is here

| | |
|---|---|
| `crates/dibs/` | the `dibs` command: the grammar, a call to a machine, placement, status, and the recipe layer behind `dibs build`, `test` and `bench`. |
| `crates/dibs-format/` | the ids, exits, records and line formats both halves read and write. |
| `crates/dibs-runner/` | what runs on the machine: the lock, the job and the status. Each machine builds the version a client needs, the first time it needs it. |
| `bin/dibs` | where the bash dibs was: for a dibs still linked here, it says how to install the binary, and runs nothing. |
| `crates/dibstop/` | `dibstop`, a live view of who holds the machines. |
| `crates/dibs-machines/` | `dibs-machines`, a desktop window on what each machine has against what it should. |
| `docs/guide.md` | the detailed guide. |
| `docs/design/` | the design records: architecture, protocol, decisions, and history. |
| `dibs-agent-rules.md` | the rules your agents follow. |
| `crates/dibs/tests/suite/` | dibs end to end, each test in a sandbox of its own. Never touches a real machine. |
| `crates/dibs/tests/live/` | the few things only a real machine can show. Runs only when asked for by name. |
| `.github/workflows/ci.yml` | formatting, clippy, the tests on Linux and macOS, the window's build, and a scan for private names whose patterns live in the `PRIVATE_STRINGS` secret. |

## Tests

    # dibs's unit tests and suite, dibs-format's and dibstop's: about 15 s, safe while others work.
    cargo test

    # dibs-machines, which a plain cargo test leaves out for its GUI toolkit.
    cargo test -p dibs-machines

    # What a call costs, timed in the sandbox, to compare one version of dibs with another.
    cargo test --test suite -- --ignored --test-threads=1 baselines

    # A real, idle machine: takes its lock and kills its own jobs there, so it is named twice.
    DIBS_LIVE_MACHINE=<machine> DIBS_LIVE_CONFIRM=<machine> cargo test --test live
