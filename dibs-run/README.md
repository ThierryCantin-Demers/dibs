# dibs-run

The agent-facing half of dibs: verbs, recipes, labels and provenance, over a resource layer
whose only job is to hand back a machine with the right things held.

It runs here rather than on the target, which is why it can be a program rather than a shell
script. The half that ships over ssh stays bash on purpose: installing nothing on a machine is
what makes adding one cheap.

    cargo build --release && cp target/release/dibs-run ~/.local/bin/

    dibs-run list  <repo>
    dibs-run bench <repo> <recipe>
    dibs-run bench <repo> <recipe> --dry-run

## Where it is

Working: recipes, derived labels, per-step locking, provenance records, `--dry-run`.

Not yet, and it refuses rather than pretending:

- **`@ref`**, because worktree setup is not implemented and honouring it silently would measure
  whatever happens to be checked out.
- **`needs`**, because the `dibs` backend knows one machine and cannot tell what is in it.
  Running a tensor-core benchmark on whatever card is free is the failure routing exists to
  prevent.
- **Per-device isolation**, for the same reason.

Those three are what the `Backend` trait exists for. A backend that knows the fleet, or `srun`
against a cluster, satisfies them without anything above `resource.rs` changing.

## Where recipes live

Three layers, each overriding the last: bundled in the binary, then a repo's own `.dibs.toml`,
then `~/.config/dibs/recipes/<repo>.toml`. Bundled so that having the binary is the whole of
the setup, which is the only arrangement that works for someone who does not share whatever
dotfile manager the recipes would otherwise sync through. Local config on top because it is the override, and it is where a recipe
lives while it is still moving.

A recipe that settles belongs in the repo, unchanged, because that is what makes a run
recoverable years later: check out the ref and read the recipe that was there. Until then the
run record carries the procedure itself alongside its fingerprint, so a local recipe is still
recoverable from the record rather than only identifiable by it.

What neither arrangement is: a spec handed over at invocation time. That is exactly as opaque
as the `/tmp` script it would replace, and 36 of the 38 exclusive jobs in the old log were that
script.

A recipe declares a procedure and names no revisions. The invocation supplies the code, and the
run record captures what it resolved to. Pinning revisions in the recipe would bind the
procedure to a moment and make it harder to rerun every year, which is the opposite of the
point.
