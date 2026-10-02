use super::{
    labels::{label_steps, run_label},
    manifest::{Isolation, Manifest, Recipe, Source, Step, Verb},
    repo::{resolve_repo, root_of},
};
use crate::worktree;
use dibs::cli::{RecipeCall, RecipeVerb};
use dibs_format::Lock;
use std::{collections::BTreeMap, fmt, path::PathBuf};

/// Why a recipe cannot be read or run as asked, said as its `Display`.
#[derive(Debug)]
pub(crate) enum RecipeError {
    /// A recipes file that cannot be read, does not parse, or declares what is refused.
    File { path: PathBuf, why: String },
    /// Neither the repo nor this computer's recipes say anything about it.
    NoRecipes {
        repo: String,
        in_repo: PathBuf,
        local: PathBuf,
    },
    /// The call asks for what the recipes do not have, or in a way they refuse.
    Refused(String),
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
            RecipeError::Refused(why) => f.write_str(why),
        }
    }
}

impl From<String> for RecipeError {
    fn from(why: String) -> RecipeError {
        RecipeError::Refused(why)
    }
}

impl From<&str> for RecipeError {
    fn from(why: &str) -> RecipeError {
        RecipeError::Refused(why.to_string())
    }
}

impl Recipe {
    /// The two ways a recipe invalidates its own measurement, refused before anything is paid
    /// for rather than found in the numbers afterwards.
    pub fn check(&self, name: &str) -> Result<(), String> {
        let variable = |v: &str| {
            v.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        if let Some(v) = self.fresh.iter().find(|v| !variable(v)) {
            return Err(format!(
                "recipe '{name}': fresh lists variables to give each run its own value, and '{v}' is not a variable name"
            ));
        }
        // Expanded unquoted on the machine so that * and ** match, which leaves no room for
        // anything the shell would read as more than a path.
        let pattern = |a: &str| {
            let rest = a.strip_prefix("$CARGO_TARGET_DIR/").unwrap_or(a);
            !rest.is_empty()
                && !rest.starts_with('/')
                && !rest.split('/').any(|c| c == "..")
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "/._-*?[]+=,@".contains(c))
        };
        if let Some(a) = self.artifacts.iter().find(|a| !pattern(a)) {
            return Err(format!(
                "recipe '{name}': artifact '{a}' has to be a path pattern inside the tree, or under $CARGO_TARGET_DIR/,\n             \
                 made of letters, digits and / . _ - * ? [ ] + = , @"
            ));
        }
        for st in &self.steps {
            // CARGO_TARGET_DIR is redirected per tree, so a relative target/ names a directory
            // the build never writes: the step reads whatever an earlier tree left there. Only
            // cargo's own output counts, since a program may keep its own under target/ too.
            let triple = |d: &str| d.matches('-').count() >= 2;
            if let Some(t) = st.run.split_whitespace().find(|w| {
                let w = w.trim_start_matches("./").trim_start_matches(['"', '\'']);
                match w.strip_prefix("target/") {
                    Some(rest) => {
                        let dir = rest.split(['/', '"', '\'']).next().unwrap_or("");
                        dir.is_empty()
                            || triple(dir)
                            || [
                                "debug",
                                "release",
                                "doc",
                                "tmp",
                                "package",
                                "criterion",
                                "nextest",
                            ]
                            .contains(&dir)
                    }
                    None => w.trim_end_matches(['"', '\'']) == "target",
                }
            }) {
                return Err(format!(
                    "recipe '{name}' names {t}, but the build writes to $CARGO_TARGET_DIR, which dibs\n             \
                     puts outside the tree. Use $CARGO_TARGET_DIR/... instead."
                ));
            }
        }
        let cargo = |st: &Step| st.run.split_whitespace().any(|w| w == "cargo");
        let built_first = self
            .steps
            .iter()
            .take_while(|st| st.lock == Lock::Shared)
            .any(cargo);
        if let Some(st) = self
            .steps
            .iter()
            .find(|st| st.lock == Lock::Exclusive && cargo(st))
            && !built_first
        {
            // shell has no steps to split, so it is told the two calls instead.
            return Err(match name {
                "shell" => format!(
                    "a command that compiles cannot take the exclusive lock, which holds the whole\n             \
                         machine for work that tolerates neighbours. Build it first without --bench:\n               \
                         dibs shell <repo>@<ref> --reason <why> -- '{} --no-run'\n             \
                         then measure with --bench.",
                    st.run
                ),
                _ => format!(
                    "recipe '{name}' compiles under the exclusive lock, which holds the whole machine\n             \
                         for work that tolerates neighbours. Split it in two:\n               \
                         [[step]] lock = \"shared\"     run = \"{} --no-run\"\n               \
                         [[step]] lock = \"exclusive\"  run = \"{}\"",
                    st.run, st.run
                ),
            });
        }
        Ok(())
    }
}

/// Becomes the command, so it keeps this terminal: prompts, Ctrl-C and the exit status are the
/// command's own rather than something relayed.
/// A recipe invocation resolved as far as it can be without a machine: which recipe, and the
/// labels its jobs are filed under.
pub(crate) struct Resolved {
    pub(crate) dir: PathBuf,
    pub(crate) repo_name: String,
    pub(crate) verb: Verb,
    pub(crate) name: String,
    pub(crate) rec: Recipe,
    pub(crate) label: String,
    pub(crate) step_labels: Vec<String>,
    pub(crate) shell_reason: Option<String>,
    pub(crate) params: BTreeMap<String, String>,
    pub(crate) tree_fresh: Vec<String>,
}

const LABEL_DERIVED: &str =
    "derived so that every run of one piece of\n  work lands in one history.";

/// A shell's own words are refused before its tree is looked for, so one try finds them all.
fn refuse_shell_words(args: &RecipeCall, repo: &str) -> Result<(), RecipeError> {
    if args.reason.is_none() {
        return Err("shell needs --reason. Most of what gets run is neither a build nor a benchmark,\n             and knowing what those were is how the next recipe gets written.".into());
    }
    if args.command.is_none() {
        return Err("shell needs -- <command>".into());
    }
    if args.params.contains_key("label") {
        let label = run_label(repo, "shell", None, args.device.as_deref());
        return Err(format!(
            "shell takes no --label: its durations are filed under {label}, {LABEL_DERIVED} A one-off that keeps coming back is a recipe to write, and its --reason is what\n  dibs gaps counts to say so."
        )
        .into());
    }
    Ok(())
}

pub(crate) fn resolve(args: &RecipeCall) -> Result<Resolved, RecipeError> {
    let found = resolve_repo(&args.repo, &root_of(args));
    if args.verb == RecipeVerb::Shell {
        let repo = found
            .as_deref()
            .map(worktree::identity)
            .unwrap_or_else(|_| args.repo.clone());
        refuse_shell_words(args, &repo)?;
    }
    let dir = found?;
    let repo_name = worktree::identity(&dir);
    let manifest = if args.verb == RecipeVerb::Shell {
        Manifest::load_any(&dir, &repo_name)?
    } else {
        Manifest::load(&dir, &repo_name)?
    };

    let shell_reason = match args.verb {
        RecipeVerb::Shell => args.reason.clone(),
        _ => None,
    };
    let shell_recipe = shell_reason.as_ref().map(|_| Recipe {
        source: Source::Local,
        needs: None,
        isolation: Isolation::Machine,
        params: BTreeMap::new(),
        fresh: Vec::new(),
        artifacts: Vec::new(),
        steps: vec![Step {
            lock: if args.bench {
                Lock::Exclusive
            } else {
                Lock::Shared
            },
            run: args.command.clone().unwrap_or_default(),
            env: BTreeMap::new(),
        }],
    });

    let verb = Verb::parse(args.verb.as_str())
        .or(if args.verb == RecipeVerb::Shell {
            Some(Verb::Build)
        } else {
            None
        })
        .ok_or_else(|| {
            format!(
                "not a verb: {} (build, test, bench, shell, raw, list, runs, gaps or friction)",
                args.verb
            )
        })?;
    let name = if shell_recipe.is_some() {
        Some("shell")
    } else {
        args.recipe.as_deref()
    }
    .ok_or_else(|| {
        let have = manifest.names(verb);
        if have.is_empty() {
            format!("{} defines no {} recipes", dir.display(), verb.as_str())
        } else {
            format!(
                "needs a recipe name; {} has: {}",
                dir.display(),
                have.join(", ")
            )
        }
    })?;
    let mut rec = shell_recipe.clone().map(Ok).unwrap_or_else(|| {
        manifest.recipe(verb, name).cloned().ok_or_else(|| {
            let have = manifest.names(verb);
            format!(
                "no {} recipe called '{name}'; {} has: {}",
                verb.as_str(),
                dir.display(),
                if have.is_empty() {
                    "none".into()
                } else {
                    have.join(", ")
                }
            )
        })
    })?;
    if rec.steps.is_empty() {
        return Err(format!("recipe '{name}' declares no steps").into());
    }

    // Derived, never supplied. A label an agent writes by hand names the run rather than the
    // kind of work, which is why 51 of 80 labels in the old history appeared exactly once and
    // filed their duration where nothing would look it up again.
    //
    // The verb is in it because a recipe name is only unique within a verb: `build cubek cuda`
    // and `test cubek cuda` are different work, and one history for both predicts each from
    // the other. Shell has no recipe name to carry.
    let label = match &shell_recipe {
        Some(_) => run_label(&repo_name, "shell", None, args.device.as_deref()),
        None => run_label(
            &repo_name,
            verb.as_str(),
            Some(name),
            args.device.as_deref(),
        ),
    };
    if args.params.contains_key("label") && !rec.params.contains_key("label") {
        return Err(format!(
            "{} {name} takes no --label: its durations are filed under {label}, {LABEL_DERIVED}",
            verb.as_str()
        )
        .into());
    }
    let params = rec
        .values(&args.params)
        .map_err(|e| format!("{name}: {e}"))?;
    rec = rec.bound(&params);
    rec.check(name)?;
    // The duration history keys on lock and label together, so a recipe's build and its
    // measurement stay apart on their own. Two steps taking the *same* lock would not, and
    // their durations would average into one meaningless number: the bimodal history that
    // made estimates useless in the first place, rebuilt deliberately.
    let step_labels = label_steps(&label, &rec.steps);
    Ok(Resolved {
        dir,
        repo_name,
        verb,
        name: name.to_string(),
        rec,
        label,
        step_labels,
        shell_reason,
        params,
        tree_fresh: manifest.tree_fresh().to_vec(),
    })
}
