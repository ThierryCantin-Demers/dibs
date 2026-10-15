/// What a flag's value is, which says what to offer after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// The flag stands alone.
    No,
    Machine,
    Card,
    Label,
    Repo,
    /// A `<repo>@<ref>`.
    Ref,
    Path,
    /// A machine, then `on` or `off`.
    Measure,
    /// Whatever the person types: seconds, a sentence, a job id.
    Free,
}

#[derive(Debug, Clone, Copy)]
pub struct Flag {
    pub names: &'static [&'static str],
    pub value: Value,
    pub about: &'static str,
}

/// A word that starts what the rest of the line means.
#[derive(Debug, Clone, Copy)]
pub struct Subcommand {
    pub name: &'static str,
    pub about: &'static str,
}

pub const SUBCOMMANDS: &[Subcommand] = &[
    Subcommand::new("build", "a repo's build recipe"),
    Subcommand::new("test", "a repo's test recipe"),
    Subcommand::new("bench", "a repo's benchmark recipe"),
    Subcommand::new("list", "what a repo defines"),
    Subcommand::new("runs", "what was measured"),
    Subcommand::new("gaps", "what fit no recipe, and what got in the way"),
    Subcommand::new("shell", "a command in a prepared worktree"),
    Subcommand::new("raw", "a command with nothing prepared"),
    Subcommand::new("with", "a command beside a repo's servers"),
    Subcommand::new("batch", "dibs calls, one per line, with one summary"),
    Subcommand::new("machines", "the machines the inventory knows"),
    Subcommand::new("status", "who holds each machine and who waits"),
    Subcommand::new("run", "a command under the shared lock"),
    Subcommand::new("out", "a job's output"),
    Subcommand::new("fetch", "a job's kept files"),
    Subcommand::new("gc", "what fills a machine's scratch"),
    Subcommand::new("friction", "report what got in the way"),
    Subcommand::new("completions", "a shell's completions for dibs"),
];

impl Subcommand {
    const fn new(name: &'static str, about: &'static str) -> Subcommand {
        Subcommand { name, about }
    }
}

/// The flags before a verb, or of a call that names none.
pub const CALL: &[Flag] = &[
    Flag::new(&["--on"], Value::Machine, "send the call to this machine"),
    Flag::new(
        &["--bench", "-b"],
        Value::No,
        "the exclusive lock, for a measurement",
    ),
    Flag::new(
        &["--peek"],
        Value::No,
        "no lock, for what is effectively free",
    ),
    Flag::new(
        &["--label"],
        Value::Label,
        "the kind of work its time is filed under",
    ),
    Flag::new(&["--max"], Value::Free, "seconds before it is stopped"),
    Flag::new(&["--wait"], Value::Free, "seconds to wait for the lock"),
    Flag::new(&["--device"], Value::Card, "the card to run on"),
    Flag::new(
        &["--new-series"],
        Value::No,
        "start this label's series again",
    ),
    Flag::new(
        &["--hold"],
        Value::No,
        "run it here while the machine's lock is held",
    ),
    Flag::new(
        &["--with"],
        Value::Free,
        "a server for the length of the call",
    ),
    Flag::new(
        &["--ready"],
        Value::Free,
        "when the --with before it is ready",
    ),
    Flag::new(
        &["--ready-within"],
        Value::Free,
        "seconds a --with has to be ready",
    ),
    Flag::new(
        &["--port"],
        Value::Free,
        "a free port the machine picks and reserves",
    ),
    Flag::new(
        &["--status", "-s"],
        Value::No,
        "who holds each machine and who waits",
    ),
    Flag::new(
        &["--watch", "-w"],
        Value::No,
        "status again every few seconds",
    ),
    Flag::new(
        &["--sync"],
        Value::Path,
        "rsync to or from a machine, under the lock",
    ),
    Flag::new(
        &["--out"],
        Value::No,
        "a job's output, or the running jobs'",
    ),
    Flag::new(&["--stream"], Value::No, "the whole output inline"),
    Flag::new(&["--fetch"], Value::Free, "a job's kept files"),
    Flag::new(
        &["--kill"],
        Value::Free,
        "stop a job by pid, or a batch by id",
    ),
    Flag::new(
        &["--anyone"],
        Value::No,
        "with --kill: someone else's job too",
    ),
    Flag::new(&["--force"], Value::No, "with --kill: without waiting"),
    Flag::new(&["--release"], Value::No, "reclaim a lock an orphan holds"),
    Flag::new(
        &["--log"],
        Value::No,
        "what ran, what it cost, what was killed",
    ),
    Flag::new(&["--gc"], Value::No, "what fills a machine's scratch"),
    Flag::new(&["--days"], Value::Free, "with --gc: how old is old"),
    Flag::new(
        &["--dry-run"],
        Value::No,
        "say what it would do, and do nothing",
    ),
    Flag::new(
        &["--check"],
        Value::Machine,
        "install the runner and say what a machine has",
    ),
    Flag::new(
        &["--write"],
        Value::No,
        "with --check: record what it found",
    ),
    Flag::new(
        &["--machines"],
        Value::No,
        "the machines the inventory knows",
    ),
    Flag::new(
        &["--measure"],
        Value::Measure,
        "whether a benchmark may run there",
    ),
    Flag::new(
        &["--forget"],
        Value::Machine,
        "drop a machine from your inventory",
    ),
    Flag::new(
        &["--which"],
        Value::No,
        "the machine a call naming none goes to",
    ),
    Flag::new(
        &["--pick"],
        Value::No,
        "where shared work naming no machine is placed",
    ),
    Flag::new(
        &["--prefer"],
        Value::Machine,
        "with --pick: the machine to prefer",
    ),
    Flag::new(
        &["--repo"],
        Value::Repo,
        "with --pick: the repo whose cache counts",
    ),
    Flag::new(&["--all"], Value::No, "every machine at once"),
    Flag::new(&["--update"], Value::No, "pull dibs and reinstall it"),
    Flag::new(&["--friction"], Value::Free, "report what got in the way"),
    Flag::new(
        &["--reply"],
        Value::Free,
        "with --friction: answer a report",
    ),
    Flag::new(&["--json"], Value::No, "machine-readable output"),
    Flag::new(&["--verbose", "-v"], Value::No, "say more"),
    Flag::new(&["--help", "-h"], Value::No, "the help"),
];

/// The flags after a recipe verb, beside whatever `--<name>` the recipe declares.
pub const RECIPE: &[Flag] = &[
    Flag::new(&["--on"], Value::Machine, "the machine to run on"),
    Flag::new(&["--device"], Value::Card, "the card to run on"),
    Flag::new(
        &["--reps"],
        Value::Free,
        "measure this many times after one build",
    ),
    Flag::new(
        &["--sweep"],
        Value::Free,
        "<name>=<a,b,c>: one point per value",
    ),
    Flag::new(&["--pin"], Value::Ref, "build against this <repo>@<ref>"),
    Flag::new(
        &["--artifacts"],
        Value::Path,
        "where the kept files are copied",
    ),
    Flag::new(
        &["--reason"],
        Value::Free,
        "why no recipe fits, for dibs gaps",
    ),
    Flag::new(&["--max"], Value::Free, "seconds before it is stopped"),
    Flag::new(&["--root"], Value::Path, "where checkouts are looked up"),
    Flag::new(&["--bench", "-b"], Value::No, "shell: the exclusive lock"),
    Flag::new(&["--anyway"], Value::No, "measure whatever binary is there"),
    Flag::new(
        &["--there"],
        Value::No,
        "with: run the command on the machine",
    ),
    Flag::new(
        &["--new-series"],
        Value::No,
        "start this label's series again",
    ),
    Flag::new(
        &["--dry-run"],
        Value::No,
        "say what it would do, and do nothing",
    ),
    Flag::new(&["--all"], Value::No, "runs: failed runs too"),
    Flag::new(&["--json"], Value::No, "machine-readable output"),
    Flag::new(&["--verbose", "-v"], Value::No, "say more"),
    Flag::new(&["--version"], Value::No, "the dibs this is"),
    Flag::new(&["--help", "-h"], Value::No, "the help"),
];

impl Flag {
    const fn new(names: &'static [&'static str], value: Value, about: &'static str) -> Flag {
        Flag {
            names,
            value,
            about,
        }
    }

    /// The flag in `flags` that `word` names.
    pub fn named(flags: &'static [Flag], word: &str) -> Option<&'static Flag> {
        flags.iter().find(|f| f.names.contains(&word))
    }
}
