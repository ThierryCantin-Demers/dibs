use crate::cli::{
    Call, CliError, Friction, Hook, Invocation, KillTarget, Mode, OutTarget, PortName, RecipeCall,
    RecipeVerb, Run, RunLock, Service, ServiceName, shell::Command,
};
use dibs_format::{Alias, BatchId, JobId, Label, MachineName};
use std::str::FromStr;

impl Invocation {
    /// Reads a dibs command line, without the program's name. Nothing is looked up: whether a
    /// machine or a card exists is for whoever acts on it.
    pub fn parse(words: &[String]) -> Result<Invocation, CliError> {
        Parser::default().read(words)
    }

    /// Reads a command line as written, before a shell expands it. A number a flag takes that is
    /// still `$` text is read as unknown rather than refused: only the shell can say what it is.
    pub fn parse_unexpanded(words: &[String]) -> Result<Invocation, CliError> {
        Parser {
            expansion: Expansion::Pending,
            ..Parser::default()
        }
        .read(words)
    }
}

/// Whether a command line's words are as a program receives them, or as a shell has yet to
/// expand them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Expansion {
    #[default]
    Done,
    Pending,
}

impl Expansion {
    /// A word the shell has still to expand, which reads as no particular value.
    pub fn pending(self, word: &str) -> bool {
        self == Expansion::Pending && word.contains('$')
    }
}

/// The mode as the flags leave it, before what follows them is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Word {
    #[default]
    Shared,
    Bench,
    Peek,
    Status,
    Watch,
    Log,
    Release,
    Gc,
    Out,
    Fetch,
    Kill,
    Check,
    Machines,
    Which,
    Pick,
    Update,
    Forget,
    Sync,
}

/// Every flag as it was read, the way the call's words leave them.
#[derive(Debug)]
struct Parser {
    word: Word,
    subcommand: bool,
    hold: bool,
    services: Vec<Service>,
    ports: Vec<PortName>,
    ready_within: u32,
    log_lines: u32,
    watch_every: u32,
    dry_run: bool,
    days: Option<String>,
    check_host: Option<String>,
    out: Option<String>,
    fetch_job: String,
    fetch_into: Option<String>,
    kill: Option<String>,
    forget: Option<String>,
    force: bool,
    anyone: bool,
    wait: Option<String>,
    max: Option<String>,
    call: Call,
    expansion: Expansion,
}

impl Default for Parser {
    fn default() -> Self {
        Parser {
            word: Word::Shared,
            subcommand: false,
            hold: false,
            services: Vec::new(),
            ports: Vec::new(),
            ready_within: Run::default().ready_within,
            log_lines: 40,
            watch_every: 5,
            dry_run: false,
            days: None,
            check_host: None,
            out: None,
            fetch_job: String::new(),
            fetch_into: None,
            kill: None,
            forget: None,
            force: false,
            anyone: false,
            wait: None,
            max: None,
            call: Call::default(),
            expansion: Expansion::Done,
        }
    }
}

/// The flags dibs had and does not any more, with what to do instead.
const REMOVED: [(&str, &str); 9] = [
    (
        "--shared",
        "shared is what a call is unless --bench says otherwise",
    ),
    (
        "--any",
        "shared work that names no machine is placed on one",
    ),
    ("--registry-sync", "the inventory is machines.toml alone"),
    ("--detach", "the detach queue was removed"),
    ("--jobs", "the detach queue was removed"),
    ("--job", "the detach queue was removed"),
    ("--cancel", "the detach queue was removed"),
    ("--abi", "dibs --check <machine> reports what a machine has"),
    ("--rsh", "--sync starts rsync's transport itself"),
];

impl Parser {
    /// Reads `[0-9]+`, the count some flags take.
    fn count(&self, word: Option<&String>) -> Option<u32> {
        if word.is_some_and(|w| self.expansion.pending(w)) {
            return Some(0);
        }
        word.filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|w| w.parse().ok())
    }

    fn read(mut self, words: &[String]) -> Result<Invocation, CliError> {
        let mut at = 0;
        while let Some(word) = words.get(at) {
            let value = words.get(at + 1);
            // A value flag takes the next word whatever it is: `--label -h` is a label.
            let need = |flag: &str| value.cloned().ok_or(CliError::needs_value(flag));
            let need_some = |flag: &str| {
                value
                    .filter(|v| !v.is_empty())
                    .cloned()
                    .ok_or(CliError::needs_value(flag))
            };
            at += 1;
            match word.as_str() {
                "--bench" | "-b" => self.word = Word::Bench,
                "--status" | "-s" => self.word = Word::Status,
                "--sync" => return self.sync(&words[at..]),
                "--watch" | "-w" => {
                    self.word = Word::Watch;
                    if let Some(n) = self.count(value) {
                        self.watch_every = n;
                        at += 1;
                    }
                }
                "--peek" => self.word = Word::Peek,
                "--hold" => self.hold = true,
                "--with" => {
                    self.services.push(Service::of(value, &self.services)?);
                    at += 1;
                }
                "--ready" => {
                    let service = self.services.last_mut().ok_or(CliError::new(
                        "dibs: --ready says when the --with before it is ready, and none comes before it",
                    ))?;
                    service.ready = Some(need("--ready")?);
                    at += 1;
                }
                "--port" => {
                    self.ports.push(PortName::of(value, &self.ports)?);
                    at += 1;
                }
                "--ready-within" => {
                    self.ready_within = self
                        .count(value)
                        .ok_or(CliError::new("dibs: --ready-within takes seconds"))?;
                    at += 1;
                }
                "-v" | "--verbose" => self.call.verbose = true,
                "--json" => self.call.json = true,
                "--release" => self.word = Word::Release,
                "--gc" => self.word = Word::Gc,
                "--friction" => return Friction::parse(&words[at..]),
                "--dry-run" => self.dry_run = true,
                "--days" => {
                    self.days = Some(need_some("--days")?);
                    at += 1;
                }
                "--log" => {
                    self.word = Word::Log;
                    if let Some(n) = self.count(value) {
                        self.log_lines = n;
                        at += 1;
                    }
                }
                "--on" => {
                    self.call.on = Some(MachineName::new(need_some("--on")?));
                    at += 1;
                }
                "--all" => self.call.all = true,
                "--forget" => {
                    self.word = Word::Forget;
                    self.forget = Some(need_some("--forget")?);
                    at += 1;
                }
                "--prefer" => {
                    self.call.prefer = Some(need_some("--prefer")?);
                    at += 1;
                }
                "--repo" => {
                    self.call.repo = Some(need_some("--repo")?);
                    at += 1;
                }
                "--pick" => self.word = Word::Pick,
                "--update" => self.word = Word::Update,
                "--which" => self.word = Word::Which,
                "--machines" => self.word = Word::Machines,
                "--write" => self.call.write = true,
                "--check" => {
                    self.word = Word::Check;
                    if let Some(host) = value.filter(|v| !v.is_empty() && !v.starts_with('-')) {
                        self.check_host = Some(host.clone());
                        at += 1;
                    }
                }
                "--out" => at += self.out(value),
                "--fetch" => at += self.fetch(&words[at..]),
                "--kill" => {
                    self.word = Word::Kill;
                    self.kill = Some(need_some("--kill")?);
                    at += 1;
                }
                "--force" => self.force = true,
                "--anyone" => self.anyone = true,
                "--wait" => {
                    self.wait = Some(need("--wait")?);
                    at += 1;
                }
                "--max" => {
                    self.max = Some(need("--max")?);
                    at += 1;
                }
                "--device" => {
                    self.call.device = Some(Alias::new(need("--device")?));
                    at += 1;
                }
                "--label" => {
                    self.call.label = Some(need("--label")?)
                        .filter(|label| !label.is_empty())
                        .map(Label::new);
                    at += 1;
                }
                "--new-series" => self.call.new_series = true,
                "--stream" => self.call.stream = true,
                "-h" | "--help" => return Ok(Invocation::Help),
                "--" => return self.rest(&words[at..]),
                flag if flag.starts_with('-') => {
                    return Err(match REMOVED.iter().find(|(gone, _)| *gone == flag) {
                        Some((gone, instead)) => {
                            CliError::new(format!("dibs: {gone} is gone: {instead}."))
                        }
                        None => CliError::with_help(format!("unknown option: {flag}")),
                    });
                }
                // The first word that is not a flag may be a subcommand; flags may come after it.
                word if !self.subcommand => {
                    self.subcommand = true;
                    let shared = self.word == Word::Shared;
                    match word {
                        "run" if matches!(self.word, Word::Shared | Word::Bench) => {}
                        "status" if shared => self.word = Word::Status,
                        "gc" if shared => self.word = Word::Gc,
                        "friction" if shared => return Friction::parse(&words[at..]),
                        "hook" if shared => return Hook::parse(&words[..at - 1], &words[at..]),
                        "out" if shared => at += self.out(words.get(at)),
                        "fetch" if shared => at += self.fetch(&words[at..]),
                        verb => match RecipeVerb::of_word(verb) {
                            Some(verb) => return self.recipe(verb, words, at),
                            None => return self.rest(&words[at - 1..]),
                        },
                    }
                }
                _ => return self.rest(&words[at - 1..]),
            }
        }
        self.rest(&[])
    }

    fn out(&mut self, value: Option<&String>) -> usize {
        self.word = Word::Out;
        match value.filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
        {
            Some(target) => {
                self.out = Some(target.clone());
                1
            }
            None => 0,
        }
    }

    /// The job is the next word, whatever it is; the directory is the one after, unless a flag.
    fn fetch(&mut self, after: &[String]) -> usize {
        self.word = Word::Fetch;
        let Some(job) = after.first() else {
            return 0;
        };
        self.fetch_job = job.clone();
        match after
            .get(1)
            .filter(|d| !d.is_empty() && !d.starts_with('-'))
        {
            Some(into) => {
                self.fetch_into = Some(into.clone());
                2
            }
            None => 1,
        }
    }

    /// Only `--on` may come before a recipe verb, which then reads everything after it.
    fn recipe(self, verb: RecipeVerb, words: &[String], at: usize) -> Result<Invocation, CliError> {
        let before = &words[..at - 1];
        match before {
            [] => RecipeCall::parse(verb, &words[at..], None, self.expansion),
            [on, machine] if on == "--on" => {
                RecipeCall::parse(verb, &words[at..], Some(machine.clone()), self.expansion)
            }
            _ => Err(CliError::new(format!(
                "dibs: only --on can come before {verb}. Put the rest after it:  dibs {verb} ... <flags>"
            ))),
        }
    }

    /// Everything after `--sync` is rsync's, the machine's side marked by a leading colon.
    fn sync(mut self, args: &[String]) -> Result<Invocation, CliError> {
        self.word = Word::Sync;
        self.refuse_hold_and_services()?;
        if args.len() < 2 {
            return Err(CliError::with_help(
                "--sync takes rsync options, a source and a destination",
            ));
        }
        const DIBS_FLAGS: [&str; 7] = [
            "--on",
            "--label",
            "--bench",
            "--device",
            "--wait",
            "--max",
            "--new-series",
        ];
        for arg in args {
            if DIBS_FLAGS.contains(&arg.as_str()) {
                return Err(CliError::new(format!(
                    "dibs: {arg} is a dibs flag, and after --sync everything is rsync's.\n  Put it before:  dibs {arg} ... --sync <opts> <src> <dst>"
                )));
            }
            if arg.starts_with(':') && arg.contains('$') {
                return Err(CliError::new(format!(
                    "dibs: --sync does not expand variables on the machine: {arg}\n  Name the directory: :~/.cache/dibs/... is $DIBS_SCRATCH, :~/... is $HOME"
                )));
            }
        }
        if !args.iter().any(|a| a.starts_with(':')) {
            return Err(CliError::new(
                "--sync: mark the machine's side with a leading colon, as in :~/.cache/dibs/x",
            ));
        }
        self.finish(Mode::Sync(args.to_vec()))
    }

    /// What the words after the flags mean for the mode the flags chose.
    fn rest(self, rest: &[String]) -> Result<Invocation, CliError> {
        let local = match self.word {
            Word::Machines => Some(Mode::Machines),
            Word::Which => Some(Mode::Which),
            Word::Pick => Some(Mode::Pick),
            Word::Update => Some(Mode::Update),
            Word::Forget => Some(Mode::Forget(MachineName::new(
                self.forget.clone().unwrap_or_default(),
            ))),
            _ => None,
        };
        if let Some(mode) = local {
            return Ok(Invocation::Call(Call { mode, ..self.call }));
        }
        self.refuse_hold_and_services()?;
        let mode = self.machine_mode(rest)?;
        self.finish(mode)
    }

    /// A mode a machine answers, refused when the words after the flags do not fit it.
    fn machine_mode(&self, rest: &[String]) -> Result<Mode, CliError> {
        let takes_nothing = |message: &str| match rest.is_empty() {
            true => Ok(()),
            false => Err(CliError::new(message)),
        };
        Ok(match self.word {
            Word::Shared | Word::Bench | Word::Peek => {
                if rest.is_empty() {
                    return Err(CliError::with_help("no command given"));
                }
                let command = Command(rest.to_vec());
                match self.word {
                    Word::Peek => Mode::Peek(command),
                    word => Mode::Run(Run {
                        lock: match word {
                            Word::Bench => RunLock::Bench,
                            _ => RunLock::Shared,
                        },
                        hold: self.hold,
                        services: self.services.clone(),
                        ports: self.ports.clone(),
                        ready_within: self.ready_within,
                        command,
                    }),
                }
            }
            Word::Status => {
                takes_nothing("status takes no command")?;
                Mode::Status
            }
            Word::Release => {
                takes_nothing("release takes no command")?;
                Mode::Release
            }
            Word::Log => {
                takes_nothing("--log takes only a line count")?;
                Mode::Log {
                    lines: self.log_lines,
                }
            }
            Word::Check => {
                takes_nothing("--check takes only a host")?;
                Mode::Check {
                    host: self.check_host.clone(),
                }
            }
            Word::Fetch => {
                takes_nothing("--fetch takes a job id and, optionally, a directory to copy into")?;
                let job = &self.fetch_job;
                if job.is_empty() || !job.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
                    return Err(CliError::new(
                        "--fetch needs a job id, the one after 'job' in the trailer",
                    ));
                }
                Mode::Fetch {
                    job: JobId::new(job.as_str()),
                    into: self.fetch_into.clone(),
                }
            }
            Word::Out => {
                takes_nothing("--out takes only a pid or a job id")?;
                Mode::Out(self.out.as_deref().map(|t| match t.parse() {
                    Ok(pid) => OutTarget::Pid(pid),
                    Err(_) => OutTarget::Job(JobId::new(t)),
                }))
            }
            Word::Watch => {
                takes_nothing("--watch takes only an interval in seconds")?;
                let every = match self.call.json {
                    true => self.watch_every.max(1),
                    false if self.watch_every >= 2 => self.watch_every,
                    false => {
                        return Err(CliError::new(
                            "--watch: 2 seconds is the floor. Anything faster costs the benchmark more than it tells you.",
                        ));
                    }
                };
                Mode::Watch { every }
            }
            Word::Gc => {
                takes_nothing(
                    "--gc takes no command: it sweeps what is under the machine's scratch",
                )?;
                let days = match &self.days {
                    None => None,
                    Some(days) => Some(
                        self.count(Some(days))
                            .ok_or(CliError::new("--days takes a number of days"))?,
                    ),
                };
                Mode::Gc {
                    days,
                    dry_run: self.dry_run,
                }
            }
            Word::Kill => Mode::Kill {
                target: match self.kill.as_deref().unwrap_or_default() {
                    pending if self.expansion.pending(pending) => KillTarget::Pid(0),
                    target => target.parse()?,
                },
                force: self.force,
                anyone: self.anyone,
            },
            Word::Machines
            | Word::Which
            | Word::Pick
            | Word::Update
            | Word::Forget
            | Word::Sync => unreachable!("answered before a machine mode is read"),
        })
    }

    /// `--hold`, `--with` and `--port` belong to a call that takes a lock.
    fn refuse_hold_and_services(&self) -> Result<(), CliError> {
        let locks = matches!(self.word, Word::Shared | Word::Bench);
        if self.hold && !locks {
            return Err(CliError::new(
                "dibs: --hold takes a lock for a command run on this computer. It goes alone or with --bench.",
            ));
        }
        if self.hold && self.call.device.is_some() && self.services.is_empty() {
            return Err(CliError::new(
                "dibs: --hold runs its command on this computer, so it cannot be pinned to a card there.",
            ));
        }
        if self.services.is_empty() && self.ports.is_empty() {
            return Ok(());
        }
        if !locks {
            return Err(CliError::new(
                "dibs: --with and --port belong to a call that takes a lock: a run, --bench, or --hold.",
            ));
        }
        self.services
            .iter()
            .try_for_each(|s| s.refuse_unknown_port(&self.ports))
    }

    /// What every mode a machine answers checks last, and the numbers it was given.
    fn finish(self, mode: Mode) -> Result<Invocation, CliError> {
        if !matches!(mode, Mode::Gc { .. }) {
            if self.dry_run {
                return Err(CliError::new(
                    "dibs: --dry-run belongs to --gc. A recipe run takes it after its verb.",
                ));
            }
            if self.days.is_some() {
                return Err(CliError::new("dibs: --days belongs to --gc."));
            }
        }
        let seconds = |flag: &str, value: &Option<String>| -> Result<Option<u64>, CliError> {
            value
                .as_deref()
                .filter(|v| !self.expansion.pending(v))
                .map(|v| {
                    v.parse()
                        .map_err(|_| CliError::new(format!("dibs: {flag} takes seconds")))
                })
                .transpose()
        };
        Ok(Invocation::Call(Call {
            mode,
            wait: seconds("--wait", &self.wait)?,
            max: seconds("--max", &self.max)?,
            ..self.call
        }))
    }
}

impl Service {
    /// A server declared by name rather than typed as `--with`, refused as the flag would be.
    pub fn declared(
        name: &str,
        command: &str,
        ready: Option<&str>,
        before: &[Service],
    ) -> Result<Service, CliError> {
        Ok(Service {
            ready: ready.map(str::to_string),
            ..Service::of(Some(&format!("{name}={command}")), before)?
        })
    }

    /// `--with name='<command>'`, refused unless the name is new and well formed.
    fn of(value: Option<&String>, before: &[Service]) -> Result<Service, CliError> {
        let given = value.map(String::as_str).unwrap_or_default();
        let (name, command) = given
            .split_once('=')
            .filter(|(name, _)| name.starts_with(|c: char| c.is_ascii_lowercase()))
            .ok_or(CliError::new(
                "dibs: --with takes name='<command>', as in --with server='./serve --port 7700'",
            ))?;
        if !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(CliError::new(format!(
                "dibs: a --with name is lowercase letters, digits and _, and '{name}' is not"
            )));
        }
        if before.iter().any(|s| s.name.0 == name) {
            return Err(CliError::new(format!(
                "dibs: two services are called {name}"
            )));
        }
        Ok(Service {
            name: ServiceName(name.to_string()),
            command: command.to_string(),
            ready: None,
        })
    }

    /// `--ready tcp:<name>` has to name a `--port`; a number needs none.
    pub fn refuse_unknown_port(&self, ports: &[PortName]) -> Result<(), CliError> {
        let Some(port) = self.ready.as_deref().and_then(|r| r.strip_prefix("tcp:")) else {
            return Ok(());
        };
        let port = port.rsplit(':').next().unwrap_or_default();
        let named = ports.iter().any(|p| p.0 == port);
        match port.bytes().all(|b| b.is_ascii_digit()) || named {
            true => Ok(()),
            false => Err(CliError::new(format!(
                "dibs: --ready tcp:{port} names no port. Use a number, or --port {port} to have one picked."
            ))),
        }
    }
}

impl PortName {
    /// A port declared by name rather than typed as `--port`, refused as the flag would be.
    pub fn declared(name: &str, before: &[PortName]) -> Result<PortName, CliError> {
        PortName::of(Some(&name.to_string()), before)
    }

    fn of(value: Option<&String>, before: &[PortName]) -> Result<PortName, CliError> {
        let name = value.map(String::as_str).unwrap_or_default();
        let well_formed = name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !well_formed {
            return Err(CliError::new(
                "dibs: --port names a port for the machine to pick, as in --port api, which the\n  service and the command then read as $DIBS_PORT_API. It does not ask for a\n  number: a number everyone writes down is the collision this avoids.",
            ));
        }
        if before.iter().any(|p| p.0 == name) {
            return Err(CliError::new(format!("dibs: two ports are called {name}")));
        }
        Ok(PortName(name.to_string()))
    }
}

impl FromStr for KillTarget {
    type Err = CliError;

    /// A batch id is `YYYYMMDD-HHMMSS-<pid>`; anything else has to be a pid.
    fn from_str(target: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = target.split('-').collect();
        let batch = matches!(parts.as_slice(), [day, time, pid]
            if day.len() == 8 && time.len() == 6 && !pid.is_empty()
                && [day, time, pid].iter().all(|p| p.bytes().all(|b| b.is_ascii_digit())));
        match (batch, target.parse()) {
            (true, _) => Ok(KillTarget::Batch(BatchId::new(target))),
            (false, Ok(pid)) => Ok(KillTarget::Pid(pid)),
            (false, Err(_)) => Err(CliError::new(format!("not a pid or a batch id: {target}"))),
        }
    }
}

impl Hook {
    /// `before` is what came ahead of the word `hook`, which a hook would drop.
    fn parse(before: &[String], words: &[String]) -> Result<Invocation, CliError> {
        if !before.is_empty() {
            return Err(CliError::new(format!(
                "dibs: hook takes no flags, so it would drop {}",
                before.join(" ")
            )));
        }
        match words {
            [kind] if kind == "ssh" => Ok(Invocation::Hook(Hook::Ssh)),
            _ => Err(CliError::new("dibs: hook takes one kind of hook: ssh")),
        }
    }
}

impl Friction {
    /// What follows `--friction`: a note is the first word alone.
    fn parse(words: &[String]) -> Result<Invocation, CliError> {
        let first = words.first().filter(|w| !w.is_empty());
        let first = first.ok_or(CliError::needs_value("--friction"))?;
        let friction = match first.as_str() {
            "--wait" => Friction::Wait,
            "--reply" => {
                let issue = words.get(1).map(String::as_str).unwrap_or_default();
                let issue = issue.trim_start_matches('#').parse().map_err(|_| {
                    CliError::new(format!("dibs: --reply needs an issue number, not {issue}"))
                })?;
                let close = match &words.get(3..).unwrap_or_default() {
                    [] => false,
                    [close] if close == "--close" => true,
                    _ => {
                        return Err(CliError::new(
                            "dibs: --reply <issue> '<answer>' takes only --close after it",
                        ));
                    }
                };
                let answer = words.get(2).cloned().unwrap_or_default();
                if answer.trim().is_empty() {
                    return Err(CliError::new("dibs: --reply needs the answer to post"));
                }
                Friction::Reply {
                    issue,
                    answer,
                    close,
                }
            }
            text => Friction::Note {
                text: text.to_string(),
            },
        };
        Ok(Invocation::Friction(friction))
    }
}
