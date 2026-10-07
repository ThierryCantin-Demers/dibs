use super::{
    base::{Isolation, Recipe, Step},
    error::{NotTaken, RecipeError, ShellWords},
    labels::run_label,
    manifest::{Manifest, Source, Verb},
    repo::Checkouts,
};
use crate::{
    cli::{RecipeCall, RecipeVerb},
    execution::Repo,
};
use dibs_format::Lock;
use std::{collections::BTreeMap, path::PathBuf};

/// A recipe invocation resolved as far as it can be without a machine: which recipe, and the
/// labels its jobs are filed under.
pub struct Resolved {
    pub dir: PathBuf,
    pub repo_name: String,
    pub verb: Verb,
    pub name: String,
    pub rec: Recipe,
    pub label: String,
    pub step_labels: Vec<String>,
    pub shell_reason: Option<String>,
    pub params: BTreeMap<String, String>,
    pub tree_fresh: Vec<String>,
}

/// A shell's own words are refused before its tree is looked for, and all in one refusal, so
/// one try finds them all.
fn refuse_shell_words(args: &RecipeCall, repo: &str) -> Result<(), RecipeError> {
    let mut wrong = Vec::new();
    if args.reason.is_none() {
        wrong.push(ShellWords::NoReason);
    }
    match (&args.command, &args.recipe) {
        (None, Some(word)) => wrong.push(ShellWords::RecipeName(word.clone())),
        (None, None) => wrong.push(ShellWords::NoCommand),
        _ => {}
    }
    if args.params.contains_key("label") {
        wrong.push(ShellWords::Label(run_label(
            repo,
            "shell",
            None,
            args.device.as_deref(),
        )));
    }
    wrong.extend(
        NotTaken::of(args.params.keys().filter(|n| *n != "label"))
            .into_iter()
            .map(ShellWords::NotTaken),
    );
    match wrong.is_empty() {
        true => Ok(()),
        false => Err(RecipeError::Shell(wrong)),
    }
}

impl Resolved {
    pub fn of(args: &RecipeCall) -> Result<Resolved, RecipeError> {
        let found = Checkouts::of(args).and_then(|checkouts| checkouts.find(&args.repo));
        if args.verb == RecipeVerb::Shell {
            let repo = found
                .as_deref()
                .map(|dir| Repo(dir).identity())
                .unwrap_or_else(|_| args.repo.clone());
            refuse_shell_words(args, &repo)?;
        }
        let dir = found?;
        let repo_name = Repo(&dir).identity();
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
            .ok_or(RecipeError::NotAVerb(args.verb))?;
        let name = if shell_recipe.is_some() {
            Some("shell")
        } else {
            args.recipe.as_deref()
        }
        .ok_or_else(|| {
            let have = manifest.names(verb);
            match have.is_empty() {
                true => RecipeError::NoneOfVerb {
                    dir: dir.clone(),
                    verb,
                },
                false => RecipeError::Unnamed {
                    dir: dir.clone(),
                    have,
                },
            }
        })?;
        let mut rec = shell_recipe.clone().map(Ok).unwrap_or_else(|| {
            manifest
                .recipe(verb, name)
                .cloned()
                .ok_or_else(|| RecipeError::NoSuchRecipe {
                    verb,
                    name: name.to_string(),
                    dir: dir.clone(),
                    have: manifest.names(verb),
                })
        })?;
        if rec.steps.is_empty() {
            return Err(RecipeError::NoSteps(name.to_string()));
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
            return Err(RecipeError::Labelled {
                verb,
                name: name.to_string(),
                label,
            });
        }
        let params = rec.values(&args.params).map_err(|why| RecipeError::Param {
            recipe: name.to_string(),
            why,
        })?;
        rec = rec.bound(&params);
        rec.check(name)?;
        // The duration history keys on lock and label together, so a recipe's build and its
        // measurement stay apart on their own. Two steps taking the *same* lock would not, and
        // their durations would average into one meaningless number: the bimodal history that
        // made estimates useless in the first place, rebuilt deliberately.
        let step_labels = rec.step_labels(&label);
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
}
