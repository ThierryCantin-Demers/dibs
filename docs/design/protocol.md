# The client and the runner

A call is two processes: the `dibs` client where the caller is, and `dibs-runner` on the machine,
which takes the lock and runs the job. This page is what passes between them and what they share
on the machine. The types are in `crates/dibs-format`.

## A call

The client starts the runner and talks to it over the runner's stdin and stdout.

- **On this computer** (`DIBS_LOCAL=1`, or the machine is this one): the client runs its own binary
  as `dibs __runner serve <hash>`. The client links the runner, so the two are always the same
  source and nothing is installed. It is a child process rather than a function call, because the
  runner's pid is what the lock records name and what `dibs --kill` signals, and one client can
  make several calls at once.
- **Over ssh:** the client asks the login shell there, whichever it is, to run

  ```
  sh -c 'r=$HOME/.cache/dibs/runner/<hash>/dibs-runner; [ -x "$r" ] || exit 125; exec "$r" serve <hash>'
  ```

  fish, bash and dash all read that line the same way. Nothing is written on the machine to run a
  call, so a full disk does not stop one.

### Frames

Every message, both ways, is one frame: a header line, then the payload.

```
<kind> <length>\n<length bytes of payload>
```

| Kind | Way | Payload |
|---|---|---|
| `request` | to the runner, first and once | the call, as JSON: mode, label, command, `--max` and where it came from, the card, the caller, the batch step, the fingerprint, how the caller is watched, and the `--port` names and `--with` servers |
| `beat` | to the runner | none: the caller is still there |
| `release` | to the runner | a held command's exit, in decimal: the end of a hold, not the caller going away |
| `out` | to the client | bytes for the caller's stdout |
| `err` | to the client | bytes for the caller's stderr |
| `record` | to the client | a fact as JSON: the job's trailer, which the client prints on stderr, or that a hold's lock is held, with the ports picked for it |
| `exit` | to the client, last | the call's exit, in decimal |

The runner's own stderr carries only what is not a frame, a panic for instance, and the client
passes it through with ssh's. A call whose runner ends without an `exit` frame exits with the
runner's status.

### A transfer

`dibs --sync` runs rsync here with `dibs __rsh` as its transport, which reaches the runner with a
request of mode `rsh`. Both streams after it are rsync's own, so the runner reads the request to
its last byte and no further, answers with one `transferring` record, and frames nothing after
it: its messages go to stderr as text, and its exit is the call's. The client carries its own
stdin and stdout across only once that record has arrived, so a runner that has to be built
first loses none of rsync's stream. The job's stdin is the runner's, and since rsync reads none
of it while it prepares a tree, the runner watches its stdout instead: whoever reads it closing
it is the caller gone. `TERM` and `HUP` to `dibs --sync` are passed to rsync, and dibs ends of
the signal once rsync has.

### Liveness

- The end of the runner's stdin means the caller is gone. So does silence longer than the
  request's lease. Over ssh the client beats every quarter lease (120 s unless `DIBS_LEASE` says);
  on this computer the lease is 0 and only the end counts, which a `SIGKILL` of the client
  produces too.
- A caller gone while queued takes its call out of the queue at once. One gone while its job runs
  has the job's whole tree stopped, deepest first, `KILL` for what outlives `TERM`; the runner then
  finishes as for any other end.
- `TERM`, `HUP` and `INT` to the runner stop the job's tree the same way and exit 143, 129 or 130.
  The runner handles them from before it writes its first record, so a signal at any moment either
  stops a job that has started or prevents it from starting. It sends the `exit` frame only if
  that waits for nothing, so a caller that stopped reading cannot keep it from ending; and it
  lets the lock go before it sends the digest and the trailer, which such a caller can hold up.
- The job runs in a process group of its own, with stdin from `/dev/null`, and never inherits the
  lock descriptors or the channel, which are all opened close-on-exec. So does each `--with`
  server, which is stopped with the job, and with the runner however it ends.
- A hold's job waits on the fifo `hold.<pid>` for its caller's `release`. The client starts the
  runner, or ssh, ignoring `INT` and `QUIT`, and the runner leaves a signal it was started
  ignoring ignored, so Ctrl-C reaches only the command run here.

## Versions

- `<hash>` is the first 16 hex digits of the SHA-256 of the runner's source tree: `dibs-runner`,
  `dibs-format`, the tree's workspace manifest and its `Cargo.lock`, laid out as the machine builds
  them. The client's `build.rs` packs the tree and computes the hash, and the client embeds both.
- A client only ever runs the runner at its own hash. There is no version field and no
  negotiation: the frames can change in any commit, because no runner ever reads a frame from a
  client built from other source.
- A runner knows its own hash: `install.sh` gives it to cargo as `DIBS_RUNNER_HASH`, which
  `dibs-format` compiles in, so a new hash rebuilds every crate of the tree whatever its files'
  times say. `dibs-runner hash` prints it. `serve <hash>` for a hash not its own says so on stderr
  and exits 125 before it reads a byte, so the client takes it for missing and has its own built
  over it. A runner a client links knows the client's hash.
- The one interface every runner keeps is `dibs-runner build <hash>`: the tree as a gzipped tar on
  stdin, text on stdout and stderr, exit 0 once `<hash>` is installed.

## Provisioning

- **A version missing on a machine** shows as exit 125 with no frame. The client then runs the
  newest runner already there (`ls -t`) as `build <hash>`, fed the tree. That runner takes the
  shared lock as an ordinary job, labelled `dibs-runner`, which unpacks the tree in
  `~/.cache/dibs/runner/.src.<hash>.<pid>` with the time of unpacking on every file (the tree is
  packed with none, and cargo judges freshness by those times) and runs the tree's own
  `install.sh`: `cargo build --locked --release` into `~/.cache/dibs/runner/.target`, which every
  version shares, then, once the binary cargo made names `<hash>`, a rename into
  `~/.cache/dibs/runner/<hash>/dibs-runner`. The client shows the build on stderr and then makes
  its call again, once.
- **Two clients building one version** both build; the rename makes the last one win, with the
  same bytes. A build that finds a runner naming its hash already installed when it gets the lock
  stops there.
- **A question asked within a bound** (every machine's status, placement, a batch's kill on every
  machine) builds nothing, since a build queues behind whatever holds the machine: it says the
  machine has no runner for this dibs yet, and exits 72.
- **A machine with no runner at all** refuses the call with exit 72, naming `dibs --check`.
- **`dibs --check <machine>`** installs the first runner: it streams the tree into a shell line that
  runs the same `install.sh`. No runner exists yet to take the lock, so perl takes it: its `flock`
  is `flock(2)` on Linux and macOS, the gate and `rw` are taken as a runner takes them, and perl
  is a shared holder labelled `dibs-runner`. It keeps `rw` to itself, close-on-exec, so nothing
  the build leaves running holds the lock, and runs the build in a process group of its own,
  which it waits for, stops on `TERM`, `HUP` or `INT` (`dibs --kill` sends it `TERM`), at the
  build's cap of 1800 s, or when its caller goes, then sweeps with `KILL`. A machine without perl
  is refused with exit 72, and one whose lock directory cannot be written with 71: a first build
  never runs unlocked.
- **A build that fails** exits 72 with its output, and nothing runs. Exit 72 is for telling the
  person: the machine needs cargo, network to crates.io once per dependency, and a toolchain at
  the workspace's `rust-version`.
- **The tree's lock file** is `crates/dibs-runner/provision/Cargo.lock`, the workspace lock cut
  down to the runner's dependencies. A test checks that every package in it has the version the
  workspace locks. After a dependency changes, copy the workspace `Cargo.lock` into an unpacked
  tree and run `cargo metadata --offline` there, which drops what the runner does not use.

## What a runner and a bash payload share

On switch day both run on one machine at once: a client that has updated sends requests to a
runner, one that has not still ships `lib/machine`. They meet only in files, and agree on every
one of them.

- **The lock.** The same directory, resolved in the same order (`DIBS_LOCK_DIR`, then
  `/dev/shm/dibs-lock` when it can be written, then the runtime directory, then `/tmp`) from the
  environment alone, which a runner's settings file cannot change, and the same two files. Both take them with `flock(2)`, which `flock(1)` uses: everyone passes through
  `gate` exclusively, an exclusive caller keeps holding `gate` while it waits for `rw`, and the job
  holds `rw`, shared or exclusive. The contention test in the suite runs a bash job and a runner
  job against one directory, in both orders, the bash side being master's `lib/machine` as old
  clients send it, kept in `crates/dibs/tests/suite/fixtures/machine-half.sh`.
- **Records.** `waiting.<pid>`, `holder.<pid>`, `batch.<pid>`, `cancelled.<id>` and the rest keep
  their names and lines, written and read through `dibs-format`'s codecs, and listed in the order
  of their file names, as the shell's glob lists them. A runner's `waiting` and `holder` lines add
  a tenth field, the job's id, after the nine a script writes; a runner reads lines of 6 to 10
  fields. A script reads the tenth into its fingerprint, which only blurs that script's own
  estimate key, so a field is only ever added at the end. Either half prunes, queues behind and
  bypasses the other's records. `dibs --kill` sends TERM to the holder alone, runner or script,
  which stops its own tree; `--force` sends KILL to the whole tree, deepest first.
- **History, log and job directories.** The same columns and files, so estimates and `dibs out`
  read across both. A runner appends to `history` and `log` holding `<file>.lock` shared, and
  rewrites either only holding it exclusively, by rename: past 4000 lines `history` keeps each
  label's newest 50 runs, and past 20000 `log` keeps its last 10000. A script appends with no
  lock and still cuts `history` to its last 500 lines, which only shortens it.
- **What differs.** A runner's job is in its own process group, where a script's shared the
  script's. A runner does not sweep old job directories as a job starts: that belongs to `--gc`.
  A runner keeps the pids it would stop in memory, so it writes no `work.<pid>`, which only the
  script that wrote one ever read.
