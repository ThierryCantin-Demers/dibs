# Recipes

How a Recipe runs, compares, and is written, cited as `dibs/recipes::<rule-name>`.

**Upstream files:** none.

## Running a Recipe

- **Ref:** the code a Recipe runs: a branch, tag or commit, `local` for the working tree with its
  uncommitted changes, `a..b` for an A/B of b against where it left a, and `a,b,c` for refs in
  turn; a ref the Machine cannot fetch is checked out here and sent.
- **Values:** what a Recipe's `params` declare, passed as `--<name> <value>`, refused outside their
  choices before anything is sent, and never split on commas.
- **Sweep:** one Call over several Values (`--sweep <name>=<a,b,c>`), one Batch with one summary.
- **Reps:** a Measurement repeated n times after one build, into one record (`--reps <n>`).
- **Pin:** a dependency's tree built in place of its published version (`--pin lib@local`),
  through a `[patch]` dibs writes and checks, and sitting beside the repo's tree under its name.
- **Artifacts:** the files a Recipe's `artifacts` names, which each step keeps and the run brings
  back (`--artifacts <dir>`, `dibs --fetch <job> [dir]`).
- **Run Record:** what `dibs runs [label]` shows of a run: when, where, each repo's commit, the
  Values, the measured step's time, the spread across Reps, and whether the Recipe changed.
- **Shell:** a one-off `dibs shell <repo>@<ref> --reason '<why>' -- '<command>'`, taking `--bench` for a
  Measurement, whose `cargo bench` or `cargo test` it builds first under the Shared Lock, and
  `--max <seconds>` to run past the default cap; without a repo it runs in the tree the agent is in.
- **Service Recipe:** a repo's declared servers, built and started on the Machine for one
  `dibs with <repo>[@<ref>] <service> -- <command>`, with `--bench` to time the command and
  `--there` to run it on the Machine against `127.0.0.1:$DIBS_PORT_<NAME>`.

### values-not-copies: A variation of a Recipe is its Values, never a copy of its command.

### sweep-is-one-call: A sweep is a Sweep, never a loop of Calls.

### ab-is-one-call: An A/B is one Call over an `a..b` or `a,b,c` Ref, never arms, target directories or an alternation written by hand.

### dependency-is-pinned: A build against an unpushed dependency is a Pin, never trees synced by hand or a `Cargo.toml` edited on the Machine.

### code-sent-as-ref: Code the Machine cannot fetch is sent as a Ref, never as a bundle, a tarball or a hand-written sync, and what the ignore rules leave out goes only through `[tree] carries`.

### artifacts-come-back: A Recipe's files come back as Artifacts, never through a step that copies them out for a Sync.

### shell-measurement-labelled: A Shell Measurement whose command changes in what does not matter, a scratch path or a timestamp, passes the same `--label` every time.

### margin-beats-spread: A difference between Refs smaller than the spread across their Reps is no result.

### autotune-is-fresh-or-shared: A Recipe whose tool keeps an autotune store in its tree, which successive `@local` runs reuse, names that store's variable in `fresh` when each run must tune anew, and its warmup absorbs the tuning.

## Writing a Recipe

### recipe-in-local-directory: A new Recipe is in `<repo>.toml` in the `local recipes` directory `dibs list <repo>` prints, or, for a repo with none yet, at the second path its refusal names.

### recipe-from-gaps: A new Recipe covers the reasons `dibs gaps` shows recurring for its repo.

### recipe-builds-then-measures: A Recipe builds under `shared` and measures under `exclusive`, in steps of their own.

### recipe-names-no-revision: A Recipe names no revision, since the Call supplies the code and the Run Record what it resolved to.

### recipe-checked-before-run: A new Recipe is read by `dibs list <repo>` and its Call run with `--dry-run` before it runs.

### others-recipe-kept: No agent changes a Recipe someone else measures with, since its history is of that procedure.

### recipe-left-uncommitted: A new Recipe is left uncommitted for the person the agent works for to commit and push.

### repo-recipe-is-repo-change: A `.dibs.toml` in a repo is a change to that repo, made like any other.

A Recipe is a `[build.<name>]`, `[test.<name>]` or `[bench.<name>]` table and its steps, each a
`lock` and a `run`, with `env` for what the command reads:

```toml
[bench.solve]
  [bench.solve.params]
  backend = { choices = ["cuda", "vulkan"], default = "cuda" }

  [[bench.solve.step]]
  lock = "shared"
  run  = "cargo bench --no-run --bench solve --features {backend}"

  [[bench.solve.step]]
  lock = "exclusive"
  run  = "cargo bench --bench solve --features {backend}"
```

The whole format, with `[tree]` and `[service]`, is in
[dibs's guide](https://github.com/ThierryCantin-Demers/dibs/blob/master/docs/guide.md#where-recipes-come-from).
