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
  sh -c 'r=$HOME/.cache/dibs/runner/<hash>/dibs-runner; [ -x "$r" ] || exit 125; [ "$("$r" hash 2>/dev/null)" = <hash> ] || exit 126; exec "$r" serve <hash>'
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
| `record` | to the client | a fact as JSON: the job's trailer, which the client prints on stderr, that a hold's lock is held, with the ports picked for it, or the tree laid out ahead of the job's command |
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
it is the caller gone. macOS cannot tell that by polling, so there its parent exiting is, the
process ssh started for the call or the client on this computer. `TERM` and `HUP` to
`dibs --sync` are passed to rsync, and dibs ends of the signal once rsync has.

### A tree

A recipe's request names the tree its job runs in: one an earlier call laid out, or one the
runner lays out at the head of the job, once it holds the lock and before the command. A recipe
so pays for one place in the queue rather than two.

- **Laying it out.** The runner fetches a ref into a ref of its own, never through the clone's one
  `FETCH_HEAD`, and adds the commit's worktree, or makes a sent tree's directory. It seeds a new
  target from a sibling's by reflinks (`FICLONE` on Linux, `clonefile` on macOS), stages the
  lockfile's packages beside it, sweeps what nobody has used, and asks cargo's git cache which
  pinned commits it lacks. A copy makes FIFOs, sockets and devices anew rather than opening them.
  A worktree with no index, which a checkout stopped partway leaves since git writes it last, is
  removed and added again unless a process works in it.
- **The layout** under the scratch and its markers (`.dibs-used`, `.dibs-tree`, `.dibs-packages`,
  `.prepare.lock`) are the ones a bash prepare left, so no build cache is rebuilt.
- **A sent tree's turn.** Its prepare holds `ws/<repo>/.<tree>.lock` from deciding what the tree
  starts from until its target is marked used, so a reseed that waited minutes for a sibling's
  build never replaces a tree another call has been handed since. A bash prepare takes no such
  lock.
- **What comes back.** What it laid out comes back as a `prepared` record, and what it says goes
  where the job's output goes. A step waiting for a git dependency, or a prepare that fails,
  exits 3 with `by=dibs` before its command.
- **Its cap.** The job's `--max` counts from when it holds the lock. A git command, a lock another
  prepare holds, or a seed's wait for a sibling's build that outlasts it ends the call 124 with
  `by=dibs`, and the command gets what is left.
- **The command** runs in the worktree with the target as `CARGO_TARGET_DIR`, or, for a transfer,
  in the directory the transfer names the worktree in.
- **Around a recipe step's command** the runner refuses a measurement whose target another tree
  built into since (exit 78, `by=dibs`), reads the machine's state, claims the target for a
  build's tree, records a successful build's lockfile, keeps the files the step names, and checks
  that a pin took (exit 3, `by=dibs`). It says what it did in a `stepped` record before the
  trailer.
- **A transfer with a tree** is framed until the tree is laid out: the `prepared` record, then
  `transferring`, after which the runner frames nothing. `dibs __rsh` writes the record to a file
  that the sync which started rsync reads.

### The build mark

A build step marks its target with `.dibs-building.<pid>`, named by the runner's pid. The runner
holds it shared and hands the descriptor to the build's command, so cargo, rustc and whatever
else the build starts hold it too, and a rustc that outlives its runner keeps it held. A build
that ends on its own removes the mark, a command's own exit 124 included. One stopped at its cap,
or by a signal (a status above 128), leaves it. The next build of that target that finds a mark
nobody holds removes the `incremental` and `.fingerprint` entries newer than it, then the mark,
since rustc reuses a stopped session's state and links with symbols missing. A bash build does
the same, its shell holding the mark as descriptor 7.

### The sweep

Every prepare and `dibs --gc` walk the scratch the same way and judge it by the same clocks:
`--gc` lists what one sweep judged, and a prepare removes the same things quietly.

- **What a prepare removes**, across every repo on the machine: trees under `ws` unused for
  `keep_days`, build caches under `target` unused for `target_keep_days`, job directories under
  `jobs` and entries under `tmp` unchanged for `keep_days`, and runner versions. `dibs --gc` also
  removes entries under `out` unchanged for `keep_days`. A prepare leaves `out`, where a person
  keeps results (`decisions.md`). Anything else under the scratch is listed and never touched.
- **The clocks.** A tree or a cache is judged by its `.dibs-used`, a job directory or an entry
  under `tmp` or `out` by its own time. A tree or cache with no `.dibs-used` is given one, dated
  now, rather than removed.
- **The locks** a prepare revives a tree or a cache under, which a sweep takes too:
  - `ws/<repo>/.prepare.lock`: a prepare holds it while it marks a tree it may reuse, and the
    nest the tree sits in.
  - `target/.<name>.lock`, beside a cache: a prepare holds it from deciding what the cache starts
    from until the cache is marked used.
  - `ws/<repo>/.<tree>.lock`, or `.<tree>-<nest>.lock` for one in a nest: a sent tree's turn.
  - `.cargo-lock`, cargo's own, in each profile of a cache: a build holds it.
- **Judged twice.** A sweep first judges every entry holding no lock, since a lock taken and let
  go can stay held a moment in a child another thread forks. What is past its clock it judges
  again holding the locks, each taken at once or not at all, and it keeps them through the
  removal: a tree under its repo's `.prepare.lock` and the turns of the sent trees in it, a cache
  under its `.<name>.lock` and every `.cargo-lock` in it, exclusively. What it cannot take is
  left for the next sweep, and `--gc` says by whom:
  - **held by a prepare**: a prepare holds one of its locks. A copy a prepare is seeding, or a
    tree it set aside, carries the prepare's process in its name (`.seed.<pid>`, `.old.<pid>`),
    and is the prepare's while that process runs.
  - **held by a build**: a build holds a `.cargo-lock` in the cache.
- **Two more fates.** An entry with no marker is **dated**, as above. A cache holding nothing but
  a sweep's own marker is **hollow**: it was dated while another sweep removed it, and goes
  under its lock whatever its date.
- **A dry run** (`dibs --gc --dry-run`) judges under the same locks but a sent tree's turn, so it
  dates unmarked entries and makes the lock files it takes. It removes nothing.
- **The lock files** go with what they guard: a sweep removes `.<name>.lock` with its cache, and a
  sent tree's turn with its tree or nest, each while it still holds it. Every taker checks, once
  it holds one, that its path still names the file it locked, and takes it again if not, so two
  prepares never hold two different files. `.prepare.lock` is never removed: a bash prepare takes
  it too, and makes no such check. A dry run's lock file beside a past cache stays until the sweep
  that removes the cache.
- **Runner versions** are kept by use: each call a version serves marks its `.dibs-used`. One a
  later version replaced goes once that mark is older than `keep_days`; the newest installed
  always stays, since it builds the next. A queued `--gc` starts its version again through
  `/proc/<pid>/exe` on Linux, which a removal cannot reach; macOS starts it by its path, which
  only a removal by hand reaches, since the call that queued it marked it used.

### Liveness

- The end of the runner's stdin means the caller is gone. So does silence longer than the
  request's lease. Over ssh the client beats every quarter lease (120 s unless `DIBS_LEASE` says);
  on this computer the lease is 0 and only the end counts, which a `SIGKILL` of the client
  produces too.
- A caller gone while queued takes its call out of the queue at once. One gone while its job, or
  its peek's command, runs has its whole tree stopped, deepest first, `KILL` for what outlives
  `TERM`; the runner then finishes as for any other end.
- `TERM`, `HUP` and `INT` to the runner stop the job's tree the same way and exit 143, 129 or 130.
  The runner handles them from before it writes its first record, so a signal at any moment either
  stops a job that has started or prevents it from starting. It sends the `exit` frame only if
  that waits for nothing, so a caller that stopped reading cannot keep it from ending; and it
  lets the lock go before it sends the digest and the trailer, which such a caller can hold up.
- The job runs in a process group of its own, with stdin from `/dev/null`, and never inherits the
  lock descriptors or the channel, which are all opened close-on-exec. So does each `--with`
  server, which is stopped with the job.
- Each such group is tethered to its runner: `dibs-runner tether`, a process in a group of its
  own started before the job, reads the job's group from a pipe, which the job writes between its
  fork and its exec, then waits for the runner's end of the pipe to close, and sends the group
  `TERM`, and `KILL` 10 s later, until nothing is left in it. The runner closes it once the job's
  first process has ended and waits for the sweep, so nothing the job left running outlives the
  call or the lock; a runner killed outright at any moment, by `KILL` or for memory, closes it
  too. What leaves the group, as a daemon that starts a session of its own does, is not swept.
- A hold's job waits on the fifo `hold.<pid>` for its caller's `release`. The client starts the
  runner, or ssh, ignoring `INT` and `QUIT`, and the runner leaves a signal it was started
  ignoring ignored, so Ctrl-C reaches only the command run here.

## Files on the machine

Each of these paths comes from the environment alone, never from a settings file, so every
account and both halves find the same ones.

- **The lock directory**: `DIBS_LOCK_DIR`, else `/dev/shm/dibs-lock` when it can be written, else
  the runtime directory, else `/tmp`.
  - `gate` and `rw`: the lock itself.
  - `waiting.<pid>` and `holder.<pid>`: a call queued, and a call holding the lock, one line
    each.
  - `batch.<pid>`: a batch step's call, with the steps still to come on this machine.
  - `cpu.<pid>`: what the last look at a holder's CPU saw, for the idle signal.
  - `hold.<pid>`: the fifo a hold's job waits on for its caller's `release`.
  - `with.<pid>`: the servers a call started.
  - `port.<n>`: a port picked for `--port`, reserved for the call whose pid it holds.
  - `cancelled.<id>`: a cancelled batch, whose later steps it refuses for a day.
- **History and log**: `/var/lib/dibs/history` and `log` when that directory can be written, so
  every account's durations and arrivals are one record, else under `~/.local/state/dibs`. Each
  has a `<file>.lock` beside it.
- **The scratch**, `~/.cache/dibs` unless `DIBS_SCRATCH` says:
  - `ws/<repo>/`: the repo's worktrees, its sent trees (`local-<key>`) and their nests,
    `.prepare.lock`, and each sent tree's turn.
  - `target/<name>/`: a build cache, with `.dibs-used`, `.dibs-tree` (the tree that made its last
    build), `.dibs-packages` (every lockfile built into it) and, while a build runs,
    `.dibs-building.<pid>`. Its turn, `.<name>.lock`, is beside it.
  - `jobs/<job>/`: a job's `log`, `meta` and `cmd`, and the files its step kept.
  - `tmp/`, where `TMPDIR` points, and `out/`, where people keep results.
- **Runners**, `~/.cache/dibs/runner/`: `<hash>/dibs-runner` with its `.dibs-used`, `.target`
  where every version is built, `.build.lock`, and while a build runs, the sent tree and its
  unpacked copy (`.tree.<hash>.<pid>.tar.gz`, `.src.<hash>.<pid>`).
- **Settings**: `/etc/dibs/runner.toml` and `~/.config/dibs/runner.toml` (A machine's settings).

On this computer the client keeps its inventory, recipes and `fleet.toml` under
`~/.config/dibs`, and its records under `~/.local/state/dibs`: `runs.jsonl`, `friction.jsonl`,
the card each label's series is on, the machine that last built each repo, the job logs already
read, and each batch's output. `crates/dibs/src/paths.rs` names every one.

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
  stdin, text on stdout and stderr, exit 0 once `<hash>` is installed. The newest runner already
  on a machine builds the next version, and may be older than the tree it is sent, so the build's
  steps live in the tree, in its `install.sh`, and not in the runner.

### What must stay compatible

- **Frames and the request:** nothing. They change freely.
- **The bootstrap line and `build <hash>`**, above: every version keeps them.
- **The files on the machine:** every version reads them, and so does the bash half until the
  last client has switched. A name keeps its meaning, a record's line only gains fields at its
  end, and the scratch layout and its markers stay, so no build cache is rebuilt.
- **The records on this computer**, `runs.jsonl` above all, are read back by every later client.

## Provisioning

- **A version missing on a machine** shows as exit 125 with no frame, or 126 or 127 when it was
  removed between the far shell's check and its `exec`. A broken login shell, or a runner that is
  there but cannot `exec` (another architecture, a missing loader, a `noexec` mount), ends the
  same way and is taken for missing too: a build is tried once and the call made once more, and
  then the call ends saying the runner could not be built there. The client then runs the
  newest runner already there (`ls -t`) as `build <hash>`, fed the tree. That runner takes the
  shared lock as an ordinary job, labelled `dibs-runner`, which unpacks the tree in
  `~/.cache/dibs/runner/.src.<hash>.<pid>` with the time of unpacking on every file (the tree is
  packed with none, and cargo judges freshness by those times) and runs the tree's own
  `install.sh`: `cargo build --locked --release` into `~/.cache/dibs/runner/.target`, which every
  version shares, then, once the binary cargo made names `<hash>`, a rename into
  `~/.cache/dibs/runner/<hash>/dibs-runner`. The far shell prints a marker line once it has found
  that runner, and only then does the client announce the build; it shows the build on stderr and
  then makes its call again, once. The build's stdin is the tree, so it watches its caller as a transfer
  does, through stdout; stopped either way, or by `TERM`, `HUP` or `INT`, it removes the tree it
  was sent and the unpacked copy.
- **Builds take turns.** Every build of the runner on a machine, the first included, holds
  `~/.cache/dibs/runner/.build.lock` exclusively from before it queues for the shared lock until
  it has installed, since cargo lets its own lock go before `install.sh` copies the binary out of
  the target every version shares. A build that finds a runner naming its hash already installed
  when its turn comes stops there.
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

## A machine's settings

A runner reads its policy from two files of flat `name = value` lines, each name a variable's
without `DIBS_`, then from its environment, then its defaults.

- **`/etc/dibs/runner.toml`**, which only root writes and every account reads, alone sets what
  every account must read alike: `bypass`, `quick`, `patience` and `machine_series`. One
  account's `quick = 600` would otherwise send its jobs around everyone's benchmarks.
  `DIBS_MACHINE_SETTINGS` names another file, for a test.
- **`~/.config/dibs/runner.toml`** in the account a runner runs as sets the rest, over the same
  names in the machine's file.
- **Where the lock and the shared files are** (`lock_dir`, `shared_lock_dir`, `shared_state_dir`,
  `history`, `log`, `scratch`) comes from the environment alone.
- A key a file may not set is ignored, and named on every call and by `dibs --check`, which also
  names every line that sets nothing a runner reads.

## Platforms

What differs between operating systems is behind one trait, in
`crates/dibs-runner/src/platform/`: Linux reads `/proc`, macOS reads libproc and sysctl. macOS
differs in five ways.

- It cannot say which process holds an flock, so `dibs status` names no orphan there, and an
  orphaned lock waits for a kill by hand.
- It shows a process's environment and working directory to its own account alone, so `LEFT
  RUNNING`, and the check that no process works in a tree about to be replaced, see only that
  account's processes.
- A queued `--gc` starts its runner by its path (The sweep).
- A pipe's reader closing wakes no `poll`, so a transfer or a build sees its caller go through
  kqueue, edge-triggered on stdout, or its parent exiting (A transfer); a first build, which
  perl runs, writes a line the client drops once a second and stops when the write fails.
- It cannot say which blocks a cloned cache shares with the one it was seeded from, so `dibs
  --gc` sizes a clone by its whole length.

## Viewers and the check

- **`dibs status`** reads the lock directory without taking the lock. Its `LEFT RUNNING` section
  lists processes whose environment names a job that has written its `meta`, which a job does as
  it ends, so each status and each watch tick reads the environment of every process it can.
  `STUCK?` marks a holder running over twice its label's 90th percentile, in `dibs status` and
  `dibstop` alike.
- **`dibstop`** keeps one watch per machine open in its own process, through the client library,
  and acts by running `dibs`. It never provisions (`architecture.md`).
- **`dibs --check` and `dibs machines`** read one probe, which the runner answers before it
  queues: it takes no lock, and runs `nvidia-smi`, `rustup` and a listing of the clones beside
  whatever is being measured. `dibs machines` probes only machines in the inventory, and says to
  record any other with `dibs --check`.

## The ssh hook

`dibs hook ssh` refuses, as a Claude Code PreToolUse hook, a shell command that would reach a
machine around its lock. It reads the command the way the shell would:

- **Names:** each machine's inventory name, its `ssh` destination's host and its `hostname`, and
  any of them with a domain after it.
- **Tools:** ssh and sftp at their destination, the first operand once the values of their
  options are skipped; scp and rsync at a `host:` or `rsync://` operand. An rsync whose `-e` is
  dibs's own transport passes.
- **Around them:** `;`, `&&`, pipes, `$( )` and backticks; the values of wrappers' options, for
  `sudo`, `env`, `nice`, `timeout`, `xargs`, `sshpass` and the like; the text of `sh`, `bash`,
  `zsh` or `dash -c`. Any other quoted string, and a heredoc's body, is text, so a commit
  message naming a machine passes.
- **What it cannot see:** a name that is in no inventory entry, as an alias in `~/.ssh/config`
  or an address; a name built at run time; git over ssh; and anything a script it runs does.
- **It fails open.** With no inventory, or input it cannot read, it knows no machine and lets the
  command through.
- **The word is taken.** A bare `dibs` runs its words as a command on a machine, except `hook`,
  as `status`, `gc`, `out`, `fetch` and `friction`. `dibs run hook` runs a command named `hook`.

## What a runner and a bash payload share

On switch day both run on one machine at once: a client that has updated sends requests to a
runner, one that has not still ships `lib/machine`. They meet only in files, and agree on every
one of them.

- **The lock.** The same directory, found the same way (Files on the machine), and the same two
  files. Both take them with `flock(2)`, which `flock(1)` uses: everyone passes through
  `gate` exclusively, an exclusive caller keeps holding `gate` while it waits for `rw`, and the job
  holds `rw`, shared or exclusive. The contention test in the suite runs a bash job and a runner
  job against one directory, in both orders, the bash side being master's `lib/machine` as old
  clients send it, kept in `crates/dibs/tests/suite/fixtures/machine-half.sh`.
- **Trees.** A bash prepare and a runner's lay out the same tree for one commit and take turns
  at the repo's `.prepare.lock` to add it; the suite runs both at once against master's prepare,
  kept in `crates/dibs/tests/suite/fixtures/bash-prepare.sh`.
- **Records.** `waiting.<pid>`, `holder.<pid>`, `batch.<pid>`, `cancelled.<id>` and the rest keep
  their names and lines, written and read through `dibs-format`'s codecs, and listed in the order
  of their file names, as the shell's glob lists them. A runner's `waiting` and `holder` lines add
  a tenth field, the job's id, after the nine a script writes; a runner reads lines of 6 to 10
  fields. A script reads the tenth into its fingerprint, which only blurs that script's own
  estimate key, so a field is only ever added at the end. Either half prunes, queues behind and
  bypasses the other's records. `dibs --kill` sends TERM to the holder alone, runner or script,
  which stops its own tree; `--force` sends KILL to the whole tree, deepest first. A pid no
  record names, whose environment names a job that has written its `meta`, is what that job
  left running: `--kill` stops it and what it started, TERM and KILL 5 s later, and refuses
  another session's, which the meta's `who` line names, without `--anyone`, and `dibs status`
  names it under `LEFT RUNNING`. macOS shows a process's environment, through
  `sysctl(KERN_PROCARGS2)`, to its own account alone, as it does the directory one works in.
- **History, log and job directories.** The same columns and files, so estimates and `dibs out`
  read across both. A runner appends to `history` and `log` holding `<file>.lock` shared, and
  rewrites either only holding it exclusively, by rename; a lock file another account made
  without group write is opened to read, which `flock` takes as well. Past 4000 lines `history`
  keeps each label's newest 50 runs, and past 20000 `log` keeps its last 10000. A script appends
  with no lock and still cuts `history` to its last 500 lines, which only shortens it.
- **What differs.**
  - A runner's job is in its own process group, where a script's shared the script's, and the
    group is swept when the job ends or its runner dies.
  - A runner does not sweep old job directories as a plain job starts. It sweeps them, with old
    trees, caches, leftovers in `tmp`, and runner versions, whenever it lays out a tree, as
    `--gc` does; results in `out` go on `--gc` alone, as with a script. A script's prepare swept
    trees and caches alone, took no lock to, and could remove one a runner's prepare was
    reviving.
  - A runner keeps the pids it would stop in memory, so it writes no `work.<pid>`, which only the
    script that wrote one ever read.
  - A runner lays a recipe's tree out itself, so a job's log holds none of a prepare's `DIBS-`
    lines.
  - A script reads `DIBS_PATIENCE` and `DIBS_QUICK` from its environment alone, so until every
    client has switched, `/etc/dibs/runner.toml` leaves them at their defaults.
  - A prepare that finds the scratch full or over quota exits 70, where a script exited with
    whatever its failing command did. A repo's name, a sent tree's key, a nest's name, a
    lockfile's token or a git database's name that is not one path component, or a fresh path
    that leaves its tree or is the tree itself, is refused with 2 before anything is laid out,
    and a ref that starts with `-` is no ref (3), before git reads it.
  - After an overrun a recipe step's end still runs: the files it names are kept, and a pin that
    did not take turns the 124 into 3. A script's `timeout` stopped both with the command.
  - A step whose tree, laid out by an earlier call, has gone since exits 127 with `by=command`,
    since bash cannot start there, where a script's `cd` exited 3.
  - `dibs --gc --days` ages old runner versions by the same days as trees.
