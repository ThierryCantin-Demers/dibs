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

`install.sh` links `dibs` into `~/.local/bin`, so pulling the clone updates it. With cargo
installed it also builds the recipe layer and `dibstop`; without cargo you still get the lock.

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
`~/.claude/CLAUDE.md` does it. The rules mention hooks that stop an agent from reaching a machine
over plain ssh or sleeping under the lock. Those live in your own config, not here.

## Learn more

- [`docs/guide.md`](docs/guide.md): every feature in detail, and why it behaves the way it does.
- `dibs --help`: every flag.
- [`dibs-design/`](dibs-design/): the decisions and the measurements behind them.

## What is here

| | |
|---|---|
| `bin/dibs` | the lock. One bash file, shipped over ssh, installs nothing on the far side. |
| `core/` | the recipe layer behind `dibs build`, `test` and `bench`: recipes, labels, worktrees, provenance. |
| `dibs-tui/` | `dibstop`, a live view of who holds the machines. |
| `dibs-report/` | builds a single-page handoff report from the sources themselves. |
| `config/chips.toml` | what to assume about a chip when no runtime can probe it. |
| `docs/guide.md` | the detailed guide. |
| `dibs-design/` | the plans, the settled decisions and their measurements. |
| `dibs-agent-rules.md` | the rules your agents follow. |
| `tests/` | the lock protocol, on a scratch directory. Never touches a real machine. |

## Tests

    bash tests/dibs-test.sh        # the protocol, locally, safe while others are working
    bash tests/dibs-live-test.sh   # needs a real machine
    cd core && cargo test
