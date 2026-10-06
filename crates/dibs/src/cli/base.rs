use crate::cli::{CliError, recipe::RecipeCall, shell::Command};
use dibs_format::{Alias, BatchId, JobId, Label, MachineName};

/// What one `dibs` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// `-h` or `--help`: the help on stdout, exit 0.
    Help,
    /// `--friction ...` or `friction ...`.
    Friction(Friction),
    /// A recipe verb and everything after it.
    Recipe(RecipeCall),
    /// `--version` after a recipe verb.
    Version,
    /// One call to a machine, or a mode that answers here.
    Call(Call),
    /// `dibs hook <kind>`: a hook of an agent's harness, reading the tool call on stdin.
    Hook(Hook),
}

/// What a hook guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    /// ssh, scp, sftp and rsync aimed at a machine in the inventory.
    Ssh,
}

/// `dibs --friction`: a note, the listener, or an answer to a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Friction {
    /// The first word after `--friction`; later words are dropped.
    Note {
        text: String,
    },
    Wait,
    Reply {
        issue: u64,
        answer: String,
        close: bool,
    },
}

/// A call and the flags that shape it. A flag the mode does not read is accepted and ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Call {
    pub mode: Mode,
    pub on: Option<MachineName>,
    pub label: Option<Label>,
    pub wait: Option<u64>,
    pub max: Option<u64>,
    pub device: Option<Alias>,
    pub verbose: bool,
    pub json: bool,
    pub all: bool,
    pub stream: bool,
    pub new_series: bool,
    /// Never typed: the recipe layer asks what would refuse a call before it builds anything.
    pub preflight: bool,
    pub write: bool,
    pub prefer: Option<String>,
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// The bare `dibs <command>`, `dibs run`, `--bench` and `--hold`.
    Run(Run),
    Peek(Command),
    Status,
    Watch {
        every: u32,
    },
    Log {
        lines: u32,
    },
    Release,
    Gc {
        days: Option<u32>,
        dry_run: bool,
    },
    Out(Option<OutTarget>),
    Fetch {
        job: JobId,
        into: Option<String>,
    },
    Kill {
        target: KillTarget,
        force: bool,
        anyone: bool,
    },
    /// `--check [host]`: the host as typed, which the inventory may know by that name.
    Check {
        host: Option<String>,
    },
    /// Everything after `--sync`, for rsync.
    Sync(Vec<String>),
    Machines,
    Which,
    Pick,
    Update,
    Forget(MachineName),
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Run(Run::default())
    }
}

/// A command run under the lock, or run here while the lock is held for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub lock: RunLock,
    /// `--hold`: the command runs on this computer while the machine's lock is held.
    pub hold: bool,
    pub services: Vec<Service>,
    pub ports: Vec<PortName>,
    /// `--ready-within`: how long a service has to become ready.
    pub ready_within: u32,
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunLock {
    #[default]
    Shared,
    Bench,
}

/// `--with <name>='<command>'`, and the `--ready` after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: ServiceName,
    pub command: String,
    pub ready: Option<String>,
}

/// A `--with` name: lowercase letters, digits and `_`, starting with a letter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceName(pub String);

/// A `--port` name, which the machine binds to a free port read as `$DIBS_PORT_<NAME>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortName(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutTarget {
    Pid(u32),
    /// A job id has a dash in it; one may be kept on this computer.
    Job(JobId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillTarget {
    Pid(u32),
    Batch(BatchId),
}

impl Default for Run {
    fn default() -> Self {
        Run {
            lock: RunLock::Shared,
            hold: false,
            services: Vec::new(),
            ports: Vec::new(),
            ready_within: 300,
            command: Command::default(),
        }
    }
}

impl Run {
    /// Refused when a service's `--ready tcp:<name>` names no `--port`.
    pub fn refuse_unknown_ports(&self) -> Result<(), CliError> {
        self.services
            .iter()
            .try_for_each(|s| s.refuse_unknown_port(&self.ports))
    }
}

impl Mode {
    /// A mode answered on this computer before any machine is asked, which reads no command.
    pub fn is_local(&self) -> bool {
        matches!(
            self,
            Mode::Machines | Mode::Which | Mode::Pick | Mode::Update | Mode::Forget(_)
        )
    }
}
