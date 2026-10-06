use super::{
    base::{Isolation, Recipe, Step},
    labels::{label_steps, run_label},
    manifest::{Manifest, Source, Verb},
    repo::{resolve_repo, root_of},
};
use crate::execution;
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

const SHELL_USAGE: &str = "dibs shell <repo>[@<ref>] --reason <why> [--bench] -- <cmd>";

/// A shell's own words are refused before its tree is looked for, and all in one refusal, so
/// one try finds them all.
fn refuse_shell_words(args: &RecipeCall, repo: &str) -> Result<(), RecipeError> {
    let mut wrong = Vec::new();
    if args.reason.is_none() {
        wrong.push("needs --reason <why>: most of what gets run is neither a build nor a benchmark,\n  and knowing what those were is how the next recipe gets written".to_string());
    }
    match (&args.command, &args.recipe) {
        (None, Some(word)) => wrong.push(format!(
            "needs -- before its command: '{word}' after the repo would be a recipe's name"
        )),
        (None, None) => wrong.push("needs -- <command>".to_string()),
        _ => {}
    }
    if args.params.contains_key("label") {
        let label = run_label(repo, "shell", None, args.device.as_deref());
        wrong.push(format!(
            "takes no --label: its durations are filed under {label}, {LABEL_DERIVED} A one-off that keeps coming back is a recipe to write, and its --reason is what\n  dibs gaps counts to say so"
        ));
    }
    match wrong.is_empty() {
        true => Ok(()),
        false => Err(format!("shell {}.\n  usage: {SHELL_USAGE}", wrong.join(";\n  and ")).into()),
    }
}

pub(crate) fn resolve(args: &RecipeCall) -> Result<Resolved, RecipeError> {
    let found = root_of(args).and_then(|root| resolve_repo(&args.repo, &root));
    if args.verb == RecipeVerb::Shell {
        let repo = found
            .as_deref()
            .map(execution::identity)
            .unwrap_or_else(|_| args.repo.clone());
        refuse_shell_words(args, &repo)?;
    }
    let dir = found?;
    let repo_name = execution::identity(&dir);
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
