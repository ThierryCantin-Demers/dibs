use super::flags::{CALL, Flag, RECIPE, Value};
use crate::cli::RecipeVerb;

/// What fits where the cursor is, read from the words typed before it and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// A subcommand: nothing has been typed that says what the line is.
    Start,
    CallFlags,
    Machines,
    /// The cards of the machine `--on` named, or of every machine when none was.
    Cards {
        machine: Option<String>,
    },
    OnOff,
    Repos,
    /// A `<repo>@<ref>` of the repo typed before the `@`.
    Refs {
        repo: String,
    },
    Recipes {
        verb: RecipeVerb,
        repo: String,
    },
    Services {
        repo: String,
    },
    /// The flags of a recipe call, and the `--<name>` the recipe declares.
    RecipeFlags {
        verb: RecipeVerb,
        repo: String,
        recipe: Option<String>,
    },
    Choices {
        verb: RecipeVerb,
        repo: String,
        recipe: String,
        param: String,
    },
    Labels,
    Shells,
    Paths,
    /// A command, a sentence or a number: only the person knows.
    Nothing,
}

impl Wanted {
    /// `typed` is every word after `dibs`, the last being the one the cursor is in.
    pub fn at(typed: &[String]) -> Wanted {
        let Some((current, before)) = typed.split_last() else {
            return Wanted::Start;
        };
        let mut on = None;
        let mut at = 0;
        while let Some(word) = before.get(at) {
            if word == "--" {
                return Wanted::Nothing;
            }
            let Some(flag) = Flag::named(CALL, word) else {
                if word.starts_with('-') {
                    at += 1;
                    continue;
                }
                return Wanted::after(word, &before[at + 1..], current, on);
            };
            if flag.names.contains(&"--sync") {
                return Wanted::Paths;
            }
            let Some(value) = before.get(at + 1).filter(|_| flag.value != Value::No) else {
                if flag.value == Value::No {
                    at += 1;
                    continue;
                }
                return Wanted::value(flag, current, on);
            };
            if flag.value == Value::Measure && before.get(at + 2).is_none() {
                return Wanted::OnOff;
            }
            if flag.names.contains(&"--on") {
                on = Some(value.clone());
            }
            at += if flag.value == Value::Measure { 3 } else { 2 };
        }
        match current.starts_with('-') {
            true => Wanted::CallFlags,
            false => Wanted::Start,
        }
    }

    /// The value the cursor is in, of a flag that takes one.
    fn value(flag: &Flag, current: &str, on: Option<String>) -> Wanted {
        match flag.value {
            Value::Machine | Value::Measure => Wanted::Machines,
            Value::Card => Wanted::Cards { machine: on },
            Value::Label => Wanted::Labels,
            Value::Repo => Wanted::Repos,
            Value::Ref => Wanted::repo_or_ref(current),
            Value::Path => Wanted::Paths,
            Value::Free | Value::No => Wanted::Nothing,
        }
    }

    /// After the first word that is not a flag: a subcommand, or the command a call runs.
    fn after(word: &str, rest: &[String], current: &str, on: Option<String>) -> Wanted {
        if word == "completions" {
            return match rest.is_empty() {
                true => Wanted::Shells,
                false => Wanted::Nothing,
            };
        }
        match RecipeVerb::of_word(word) {
            Some(verb) => Wanted::in_recipe(verb, rest, current, on),
            None if matches!(word, "status" | "gc") && current.starts_with('-') => {
                Wanted::CallFlags
            }
            None => Wanted::Nothing,
        }
    }

    fn in_recipe(verb: RecipeVerb, rest: &[String], current: &str, on: Option<String>) -> Wanted {
        let mut on = on;
        let mut positional: Vec<&String> = Vec::new();
        let mut at = 0;
        while let Some(word) = rest.get(at) {
            if word == "--" {
                return Wanted::Nothing;
            }
            if let Some(flag) = Flag::named(RECIPE, word) {
                let takes = flag.value != Value::No;
                match rest.get(at + 1) {
                    None if takes => return Wanted::value(flag, current, on),
                    Some(value) if flag.names.contains(&"--on") => on = Some(value.clone()),
                    _ => {}
                }
                at += 1 + usize::from(takes);
                continue;
            }
            if let Some(param) = word.strip_prefix("--") {
                if rest.get(at + 1).is_none() {
                    return match (positional.first(), positional.get(1)) {
                        (Some(repo), Some(recipe)) => Wanted::Choices {
                            verb,
                            repo: repo_of(repo),
                            recipe: (*recipe).clone(),
                            param: param.to_string(),
                        },
                        _ => Wanted::Nothing,
                    };
                }
                at += 2;
                continue;
            }
            if !word.starts_with('-') {
                positional.push(word);
            }
            at += 1;
        }
        let repo = positional.first().map(|r| repo_of(r)).unwrap_or_default();
        if current.starts_with('-') {
            return Wanted::RecipeFlags {
                verb,
                repo,
                recipe: positional.get(1).map(|r| (*r).clone()),
            };
        }
        match (verb, positional.len()) {
            (RecipeVerb::Runs, 0) => Wanted::Labels,
            (RecipeVerb::Machines, 0) => Wanted::Machines,
            (RecipeVerb::Batch, 0) => Wanted::Paths,
            (
                RecipeVerb::Build
                | RecipeVerb::Test
                | RecipeVerb::Bench
                | RecipeVerb::List
                | RecipeVerb::Shell
                | RecipeVerb::With,
                0,
            ) => Wanted::repo_or_ref(current),
            (RecipeVerb::Build | RecipeVerb::Test | RecipeVerb::Bench, 1) => {
                Wanted::Recipes { verb, repo }
            }
            (RecipeVerb::With, 1) => Wanted::Services { repo },
            _ => Wanted::Nothing,
        }
    }

    fn repo_or_ref(current: &str) -> Wanted {
        match current.split_once('@') {
            Some((repo, _)) => Wanted::Refs {
                repo: repo.to_string(),
            },
            None => Wanted::Repos,
        }
    }
}

/// The repo of a `<repo>@<ref>`.
fn repo_of(word: &str) -> String {
    word.split_once('@')
        .map_or(word, |(repo, _)| repo)
        .to_string()
}
