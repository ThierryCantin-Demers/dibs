use crate::cli::{CliError, Invocation, parse::Expansion, shell::ShellWord};
use dibs_format::MachineName;
use std::{collections::BTreeMap, fmt, path::PathBuf};

/// The words after which the recipe layer reads the rest of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecipeVerb {
    Build,
    Test,
    Bench,
    List,
    Runs,
    Gaps,
    Shell,
    Raw,
    Batch,
    With,
    Machines,
}

impl RecipeVerb {
    const ALL: [RecipeVerb; 11] = [
        RecipeVerb::Build,
        RecipeVerb::Test,
        RecipeVerb::Bench,
        RecipeVerb::List,
        RecipeVerb::Runs,
        RecipeVerb::Gaps,
        RecipeVerb::Shell,
        RecipeVerb::Raw,
        RecipeVerb::Batch,
        RecipeVerb::With,
        RecipeVerb::Machines,
    ];

    pub fn of_word(word: &str) -> Option<RecipeVerb> {
        RecipeVerb::ALL.into_iter().find(|v| v.as_str() == word)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RecipeVerb::Build => "build",
            RecipeVerb::Test => "test",
            RecipeVerb::Bench => "bench",
            RecipeVerb::List => "list",
            RecipeVerb::Runs => "runs",
            RecipeVerb::Gaps => "gaps",
            RecipeVerb::Shell => "shell",
            RecipeVerb::Raw => "raw",
            RecipeVerb::Batch => "batch",
            RecipeVerb::With => "with",
            RecipeVerb::Machines => "machines",
        }
    }

    /// Whether it runs without a repo named after it.
    /// Whether the verb runs a recipe's jobs, or answers from what is recorded here.
    pub fn runs_jobs(self) -> bool {
        matches!(
            self,
            RecipeVerb::Build
                | RecipeVerb::Test
                | RecipeVerb::Bench
                | RecipeVerb::Shell
                | RecipeVerb::Raw
        )
    }

    fn needs_no_repo(self) -> bool {
        matches!(
            self,
            RecipeVerb::Runs | RecipeVerb::Gaps | RecipeVerb::Raw | RecipeVerb::Machines
        )
    }

    /// Whether what follows it is a `<repo>@<ref>`, rather than a label or a file whose `@`
    /// belongs to it.
    fn takes_a_ref(self) -> bool {
        !matches!(self, RecipeVerb::Runs | RecipeVerb::Batch)
    }
}

impl fmt::Display for RecipeVerb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `--sweep <name>=<a,b,c>`: one point per value. Spelled apart from `--<name>` because a value
/// may contain a comma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sweep {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeCall {
    pub verb: RecipeVerb,
    pub repo: String,
    pub reference: Option<String>,
    pub recipe: Option<String>,
    /// `--root`, where checkouts are looked up when it is given.
    pub root: Option<PathBuf>,
    pub dry_run: bool,
    pub reason: Option<String>,
    /// Everything after `--`: one word as it is, several each quoted, since it is one string on
    /// the far side.
    pub command: Option<String>,
    /// The card to run on, named from the machine's inventory.
    pub device: Option<String>,
    /// `--on`, after the verb or else before it.
    pub on: Option<String>,
    /// `--<name> <value>` for whatever the recipe declares, which is the recipe's to check.
    pub params: BTreeMap<String, String>,
    pub sweep: Vec<Sweep>,
    pub reps: u32,
    /// Where the files a recipe keeps are copied once fetched.
    pub artifacts_to: Option<String>,
    /// `<repo>@<ref>` trees to build against in place of what the lockfile names.
    pub pins: Vec<String>,
    /// shell only: the exclusive lock, for a one-off that is a measurement.
    pub bench: bool,
    pub max: Option<u64>,
    /// Measure even when another tree built into the target after this one did.
    pub anyway: bool,
    /// with only: the command runs on the machine, in the tree, rather than here.
    pub there: bool,
    pub json: bool,
    /// runs only: failed runs too.
    pub all: bool,
    /// The measurement starts its label's series on this machine again, on another card.
    pub new_series: bool,
    pub verbose: bool,
}

impl RecipeCall {
    /// The machine `--on` names.
    pub fn machine(&self) -> Option<MachineName> {
        self.on.as_deref().map(MachineName::new)
    }

    /// A call sent to a named machine, by `--on` or `DIBS_ON`, is not ranked, and says nothing
    /// about where the repo's cache belongs.
    pub fn pinned(&self) -> bool {
        self.on.is_some() || std::env::var("DIBS_ON").is_ok_and(|m| !m.is_empty())
    }

    /// Reads a recipe verb and what follows it; `on` is a `--on` given before the verb.
    pub fn parse(
        verb: RecipeVerb,
        words: &[String],
        on: Option<String>,
        expansion: Expansion,
    ) -> Result<Invocation, CliError> {
        let refused = |message: String| CliError::new(format!("dibs: {message}"));
        let mut call = RecipeCall {
            verb,
            repo: String::new(),
            reference: None,
            recipe: None,
            root: None,
            dry_run: false,
            reason: None,
            command: None,
            device: None,
            on,
            params: BTreeMap::new(),
            sweep: Vec::new(),
            reps: 1,
            artifacts_to: None,
            pins: Vec::new(),
            bench: false,
            max: None,
            anyway: false,
            there: false,
            json: false,
            all: false,
            new_series: false,
            verbose: false,
        };
        let mut positional: Vec<String> = Vec::new();
        let mut it = words.iter();
        while let Some(word) = it.next() {
            let mut value = |message: &str| it.next().cloned().ok_or(refused(message.into()));
            match word.as_str() {
                "--" => {
                    call.command = Some(match it.as_slice() {
                        [] => return Err(refused("-- needs a command after it".into())),
                        [one] => one.clone(),
                        words => words
                            .iter()
                            .map(|w| ShellWord(w).to_string())
                            .collect::<Vec<_>>()
                            .join(" "),
                    });
                    break;
                }
                "--reason" => call.reason = Some(value("--reason needs a sentence")?),
                "--device" => call.device = Some(value("--device needs an alias")?),
                "--on" => call.on = Some(value("--on needs a machine")?),
                "-h" | "--help" => return Ok(Invocation::Help),
                "--version" => return Ok(Invocation::Version),
                "--root" => call.root = Some(PathBuf::from(value("--root needs a path")?)),
                "--sweep" => {
                    let s = value("--sweep needs <name>=<value,value,...>")?;
                    let (name, values) = s.split_once('=').ok_or_else(|| {
                        refused(format!("--sweep takes <name>=<value,value,...>, not {s}"))
                    })?;
                    call.sweep.push(Sweep {
                        name: name.to_string(),
                        values: values.split(',').map(str::to_string).collect(),
                    });
                }
                "--reps" => {
                    call.reps = it
                        .next()
                        .and_then(|n| match expansion.pending(n) {
                            true => Some(1),
                            false => n.parse().ok(),
                        })
                        .filter(|n| *n > 0)
                        .ok_or(refused("--reps needs a count".into()))?;
                }
                "--artifacts" => call.artifacts_to = Some(value("--artifacts needs a directory")?),
                "--pin" => call.pins.push(value("--pin needs <repo>@<ref>")?),
                "--bench" | "-b" => call.bench = true,
                "--max" => {
                    call.max = match it.next() {
                        Some(n) if expansion.pending(n) => None,
                        n => Some(
                            n.and_then(|n| n.parse().ok())
                                .ok_or(refused("--max needs seconds".into()))?,
                        ),
                    };
                }
                "--anyway" => call.anyway = true,
                "--there" => call.there = true,
                "--json" => call.json = true,
                "--all" => call.all = true,
                "--new-series" => call.new_series = true,
                "--dry-run" => call.dry_run = true,
                "--verbose" | "-v" => call.verbose = true,
                "-" => positional.push("-".into()),
                flag if flag.starts_with("--") => {
                    let (name, value) = match flag.split_once('=') {
                        Some((name, value)) => (name, Some(value.to_string())),
                        None => (flag, None),
                    };
                    let name = name.trim_start_matches('-').to_string();
                    let value =
                        match value {
                            Some(value) => value,
                            None => it.next().filter(|v| !v.starts_with("--")).cloned().ok_or(
                                refused(format!(
                                    "--{name} needs a value, or is not a flag dibs has"
                                )),
                            )?,
                        };
                    call.params.insert(name, value);
                }
                flag if flag.starts_with('-') => {
                    return Err(refused(format!("unknown option: {flag}")));
                }
                word => positional.push(word.to_string()),
            }
        }
        let target = match positional.first() {
            Some(target) => target.clone(),
            None if verb == RecipeVerb::Shell => ".@local".to_string(),
            None => String::new(),
        };
        if target.is_empty() && !verb.needs_no_repo() {
            return Err(refused("needs a repo".into()));
        }
        if verb == RecipeVerb::With && call.command.is_none() {
            return Err(refused("with runs a command here against the repo's servers: dibs with <repo>[@<ref>] <service> -- <command>".into()));
        }
        (call.repo, call.reference) = match target.split_once('@') {
            Some((repo, reference)) if verb.takes_a_ref() => {
                (repo.to_string(), Some(reference.to_string()))
            }
            _ => (target, None),
        };
        call.recipe = positional.get(1).cloned();
        Ok(Invocation::Recipe(call))
    }
}
