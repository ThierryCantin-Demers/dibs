use super::manifest::Verb;
use crate::{cli::RecipeVerb, inventory::InventoryError, paths::FileError};
use std::{fmt, io, path::PathBuf};

const LABEL_DERIVED: &str =
    "derived so that every run of one piece of\n  work lands in one history.";

const SHELL_USAGE: &str = "dibs shell <repo>[@<ref>] --reason <why> [--bench] -- <cmd>";

/// Why a recipe cannot be read or run as asked, said as its `Display`.
#[derive(Debug)]
pub enum RecipeError {
    /// A recipes file that cannot be read, does not parse, or declares what is refused.
    File {
        path: PathBuf,
        why: ManifestError,
    },
    /// Neither the repo nor this computer's recipes say anything about it.
    NoRecipes {
        repo: String,
        in_repo: PathBuf,
        local: PathBuf,
    },
    Repo(RepoError),
    /// Everything wrong with a shell's own words, at once.
    Shell(Vec<ShellWords>),
    NotAVerb(RecipeVerb),
    NoneOfVerb {
        dir: PathBuf,
        verb: Verb,
    },
    Unnamed {
        dir: PathBuf,
        have: Vec<String>,
    },
    NoSuchRecipe {
        verb: Verb,
        name: String,
        dir: PathBuf,
        have: Vec<String>,
    },
    NoSteps(String),
    /// `--label` given to a recipe whose label is derived.
    Labelled {
        verb: Verb,
        name: String,
        label: String,
    },
    Param {
        recipe: String,
        why: ParamError,
    },
    /// A recipe that would invalidate its own measurement.
    Unsound {
        recipe: String,
        flaw: Flaw,
    },
}

/// Why a recipes file is refused.
#[derive(Debug)]
pub enum ManifestError {
    Read(io::Error),
    Parse(Box<toml::de::Error>),
    /// `needs`, which nothing routes on.
    Needs {
        recipe: String,
        needs: String,
    },
    /// A `[tree] fresh` path that is not inside the tree.
    Fresh(String),
}

/// What is wrong with a shell's words.
#[derive(Debug)]
pub enum ShellWords {
    NoReason,
    /// A word after the repo with no `--` before it, which reads as a recipe's name.
    RecipeName(String),
    NoCommand,
    /// `--label`, with the label its durations are filed under.
    Label(String),
    NotTaken(NotTaken),
}

/// A `--name value` given to a one-off, which has no recipe to declare it and would drop it.
#[derive(Debug)]
pub enum NotTaken {
    Value(String),
    /// What only `dibs run` acts on: `--with`, `--port`, `--ready` and `--ready-within`.
    Server(Vec<String>),
}

impl NotTaken {
    pub fn of<'a>(names: impl Iterator<Item = &'a String>) -> Vec<NotTaken> {
        let (served, other): (Vec<&String>, Vec<&String>) =
            names.partition(|n| matches!(n.as_str(), "with" | "port" | "ready" | "ready-within"));
        let mut not_taken: Vec<NotTaken> = other
            .into_iter()
            .map(|n| NotTaken::Value(n.clone()))
            .collect();
        if !served.is_empty() {
            not_taken.push(NotTaken::Server(served.into_iter().cloned().collect()));
        }
        not_taken
    }
}

/// Why the parameters given do not fit the recipe.
#[derive(Debug)]
pub enum ParamError {
    TakesNone(String),
    Unknown {
        name: String,
        have: Vec<String>,
    },
    NoDefault(String),
    NotAChoice {
        name: String,
        value: String,
        choices: Vec<String>,
    },
}

/// The ways a recipe invalidates its own measurement.
#[derive(Debug)]
pub enum Flaw {
    /// A `fresh` entry that is not a variable name.
    Fresh(String),
    Artifact(String),
    /// A relative `target/` path, which the build never writes.
    Target(String),
    /// A compile under the exclusive lock, with the step's command.
    CompilesExclusive(String),
}

/// Why the repo a recipe call names could not be found.
#[derive(Debug)]
pub enum RepoError {
    Inventory(InventoryError),
    /// A path in no checkout and with no `.dibs.toml`.
    NoCheckout(String),
    NotFound {
        repo: String,
        root: PathBuf,
    },
    File(FileError),
}

impl From<RepoError> for RecipeError {
    fn from(e: RepoError) -> RecipeError {
        RecipeError::Repo(e)
    }
}

impl From<InventoryError> for RepoError {
    fn from(e: InventoryError) -> RepoError {
        RepoError::Inventory(e)
    }
}

impl From<FileError> for RepoError {
    fn from(e: FileError) -> RepoError {
        RepoError::File(e)
    }
}

impl fmt::Display for RecipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecipeError::File { path, why } => write!(f, "{}: {why}", path.display()),
            RecipeError::NoRecipes {
                repo,
                in_repo,
                local,
            } => write!(
                f,
                "no recipes for {repo}: nothing in {} or {}",
                in_repo.display(),
                local.display()
            ),
            RecipeError::Repo(e) => e.fmt(f),
            RecipeError::Shell(wrong) => {
                f.write_str("shell ")?;
                for (i, w) in wrong.iter().enumerate() {
                    if i > 0 {
                        f.write_str(";\n  and ")?;
                    }
                    w.fmt(f)?;
                }
                write!(f, ".\n  usage: {SHELL_USAGE}")
            }
            RecipeError::NotAVerb(verb) => write!(
                f,
                "not a verb: {verb} (build, test, bench, shell, raw, list, runs, gaps or friction)"
            ),
            RecipeError::NoneOfVerb { dir, verb } => {
                write!(f, "{} defines no {} recipes", dir.display(), verb.as_str())
            }
            RecipeError::Unnamed { dir, have } => write!(
                f,
                "needs a recipe name; {} has: {}",
                dir.display(),
                have.join(", ")
            ),
            RecipeError::NoSuchRecipe {
                verb,
                name,
                dir,
                have,
            } => write!(
                f,
                "no {} recipe called '{name}'; {} has: {}",
                verb.as_str(),
                dir.display(),
                match have.is_empty() {
                    true => "none".to_string(),
                    false => have.join(", "),
                }
            ),
            RecipeError::NoSteps(name) => write!(f, "recipe '{name}' declares no steps"),
            RecipeError::Labelled { verb, name, label } => write!(
                f,
                "{} {name} takes no --label: its durations are filed under {label}, {LABEL_DERIVED}",
                verb.as_str()
            ),
            RecipeError::Param { recipe, why } => write!(f, "{recipe}: {why}"),
            RecipeError::Unsound { recipe, flaw } => flaw.say(recipe, f),
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Read(e) => e.fmt(f),
            ManifestError::Parse(e) => e.fmt(f),
            ManifestError::Needs { recipe, needs } => write!(
                f,
                "recipe {recipe} needs '{needs}', which nothing here can check or route on; name the machine with --on"
            ),
            ManifestError::Fresh(p) => write!(
                f,
                "[tree] fresh lists paths inside the tree, and '{p}' is not one: relative, with no . or .. \
                 part, in letters, digits and ._-/"
            ),
        }
    }
}

impl fmt::Display for ShellWords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShellWords::NoReason => f.write_str(
                "needs --reason <why>: most of what gets run is neither a build nor a benchmark,\n  and knowing what those were is how the next recipe gets written",
            ),
            ShellWords::RecipeName(word) => write!(
                f,
                "needs -- before its command: '{word}' after the repo would be a recipe's name"
            ),
            ShellWords::NoCommand => f.write_str("needs -- <command>"),
            ShellWords::Label(label) => write!(
                f,
                "takes no --label: its durations are filed under {label}, {LABEL_DERIVED} A one-off that keeps coming back is a recipe to write, and its --reason is what\n  dibs gaps counts to say so"
            ),
            ShellWords::NotTaken(not_taken) => not_taken.fmt(f),
        }
    }
}

impl fmt::Display for NotTaken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotTaken::Value(name) => write!(f, "takes no --{name}"),
            NotTaken::Server(names) => {
                let flags: Vec<String> = names.iter().map(|n| format!("--{n}")).collect();
                write!(
                    f,
                    "starts no server, so it takes no {}: dibs run does,\n  as dibs run --port <name> --with <name>='<server>' --ready tcp:<name> -- '<cmd>'",
                    flags.join(", ")
                )
            }
        }
    }
}

impl fmt::Display for ParamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParamError::TakesNone(name) => write!(
                f,
                "this recipe takes no parameters, so --{name} means nothing to it"
            ),
            ParamError::Unknown { name, have } => write!(
                f,
                "no parameter '{name}'; this recipe takes: {}",
                have.join(", ")
            ),
            ParamError::NoDefault(name) => {
                write!(f, "--{name} has no default, so it has to be given")
            }
            ParamError::NotAChoice {
                name,
                value,
                choices,
            } => write!(f, "--{name} {value} is not one of: {}", choices.join(", ")),
        }
    }
}

impl Flaw {
    fn say(&self, recipe: &str, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Flaw::Fresh(v) => write!(
                f,
                "recipe '{recipe}': fresh lists variables to give each run its own value, and '{v}' is not a variable name"
            ),
            Flaw::Artifact(a) => write!(
                f,
                "recipe '{recipe}': artifact '{a}' has to be a path pattern inside the tree, or under $CARGO_TARGET_DIR/,\n             \
                 made of letters, digits and / . _ - * ? [ ] + = , @"
            ),
            Flaw::Target(t) => write!(
                f,
                "recipe '{recipe}' names {t}, but the build writes to $CARGO_TARGET_DIR, which dibs\n             \
                 puts outside the tree. Use $CARGO_TARGET_DIR/... instead."
            ),
            // shell has no steps to split, so it is told the two calls instead.
            Flaw::CompilesExclusive(run) if recipe == "shell" => write!(
                f,
                "a command that compiles cannot take the exclusive lock, which holds the whole\n             \
                 machine for work that tolerates neighbours. Build it first without --bench:\n               \
                 dibs shell <repo>@<ref> --reason <why> -- '{run} --no-run'\n             \
                 then measure with --bench."
            ),
            Flaw::CompilesExclusive(run) => write!(
                f,
                "recipe '{recipe}' compiles under the exclusive lock, which holds the whole machine\n             \
                 for work that tolerates neighbours. Split it in two:\n               \
                 [[step]] lock = \"shared\"     run = \"{run} --no-run\"\n               \
                 [[step]] lock = \"exclusive\"  run = \"{run}\""
            ),
        }
    }
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepoError::Inventory(e) => e.fmt(f),
            RepoError::NoCheckout(repo) => write!(
                f,
                "'{repo}' is in no git checkout and has no .dibs.toml, so there is no tree to send"
            ),
            RepoError::NotFound { repo, root } => write!(
                f,
                "no repo at '{repo}' and none at {}/{repo}.\n  A bare name is looked up under DIBS_ROOT, then --root, then the `root` key of\n  ~/.config/dibs/machines.toml, then the current directory. Give a path, or set one of those.",
                root.display()
            ),
            RepoError::File(e) => e.fmt(f),
        }
    }
}
