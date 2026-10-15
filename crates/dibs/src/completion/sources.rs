use super::{
    Wanted,
    flags::{CALL, Flag, RECIPE, SUBCOMMANDS},
};
use crate::{
    cli::RecipeVerb,
    execution::Repo,
    git::Git,
    inventory::{Inventory, Machine},
    recipe::{Checkouts, Manifest, Verb},
    records::RunLog,
};
use dibs_format::Moment;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// One word that fits, and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub value: String,
    pub about: String,
}

/// What this computer knows that a completion can offer: its inventory, its recipes, its
/// checkouts and its runs. Anything that cannot be read offers nothing rather than an error.
pub struct Sources {
    inventory: Option<Inventory>,
    checkouts: Option<Checkouts>,
}

impl Sources {
    pub fn here() -> Sources {
        Sources {
            inventory: Inventory::here().ok().flatten(),
            checkouts: Checkouts::here().ok(),
        }
    }

    pub fn candidates(&self, wanted: &Wanted) -> Vec<Candidate> {
        match wanted {
            Wanted::Start => SUBCOMMANDS
                .iter()
                .map(|s| Candidate::new(s.name, s.about))
                .collect(),
            Wanted::CallFlags => flags(CALL),
            Wanted::Machines => self.machines(),
            Wanted::Cards { machine } => self.cards(machine.as_deref()),
            Wanted::OnOff => vec![
                Candidate::new("on", "a benchmark may run there"),
                Candidate::new("off", "a benchmark is refused there"),
            ],
            Wanted::Repos => self.repos(),
            Wanted::Refs { repo } => self.refs(repo),
            Wanted::Recipes { verb, repo } => self.recipes(*verb, repo),
            Wanted::Services { repo } => self
                .manifest(repo)
                .service_listing()
                .into_iter()
                .map(|s| Candidate::new(s.name, "a server the repo declares"))
                .collect(),
            Wanted::RecipeFlags { verb, repo, recipe } => {
                let mut found = self.params(*verb, repo, recipe.as_deref());
                found.extend(flags(RECIPE));
                found
            }
            Wanted::Choices {
                verb,
                repo,
                recipe,
                param,
            } => self.choices(*verb, repo, recipe, param),
            Wanted::Labels => RunLog::here()
                .and_then(|log| log.runs())
                .map(|runs| runs.labels())
                .unwrap_or_default()
                .into_iter()
                .map(|label| Candidate::new(&label, ""))
                .collect(),
            Wanted::Shells => vec![Candidate::new("fish", ""), Candidate::new("zsh", "")],
            Wanted::Paths | Wanted::Nothing => Vec::new(),
        }
    }

    fn machines(&self) -> Vec<Candidate> {
        self.inventory
            .iter()
            .flat_map(|i| &i.machines)
            .map(|m| Candidate::new(m.name.as_str(), &described(m)))
            .collect()
    }

    fn cards(&self, machine: Option<&str>) -> Vec<Candidate> {
        let mut seen = BTreeSet::new();
        self.inventory
            .iter()
            .flat_map(|i| &i.machines)
            .filter(|m| machine.is_none_or(|name| m.name.as_str() == name))
            .flat_map(|m| m.devices.iter().map(move |d| (m, d)))
            .filter_map(|(m, d)| {
                let alias = d.alias.as_ref()?.as_str().to_string();
                let card = d.name.clone().unwrap_or_default();
                let about = match machine {
                    Some(_) => card,
                    None => format!("{card} on {}", m.name),
                };
                seen.insert(alias.clone())
                    .then(|| Candidate::new(&alias, &about))
            })
            .collect()
    }

    /// Repos with recipes on this computer, then checkouts under the root that carry their own.
    fn repos(&self) -> Vec<Candidate> {
        let mut repos: BTreeSet<String> = Manifest::local_repos().into_iter().collect();
        if let Some(root) = self.checkouts.as_ref().map(Checkouts::root)
            && let Ok(entries) = std::fs::read_dir(root)
        {
            repos.extend(
                entries
                    .flatten()
                    .filter(|e| e.path().join(".dibs.toml").is_file())
                    .filter_map(|e| e.file_name().to_str().map(str::to_string)),
            );
        }
        repos.into_iter().map(|r| Candidate::new(&r, "")).collect()
    }

    fn refs(&self, repo: &str) -> Vec<Candidate> {
        let mut found = vec![Candidate::new(
            &format!("{repo}@local"),
            "your working tree, uncommitted changes included",
        )];
        let Some(dir) = self.checkout(repo) else {
            return found;
        };
        let names = Git(&dir)
            .run(&[
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads/",
                "refs/remotes/origin/",
            ])
            .unwrap_or_default();
        found.extend(
            names
                .lines()
                .filter(|name| *name != "origin" && !name.ends_with("/HEAD"))
                .map(|name| Candidate::new(&format!("{repo}@{name}"), "")),
        );
        found
    }

    fn recipes(&self, verb: RecipeVerb, repo: &str) -> Vec<Candidate> {
        let Some(verb) = Verb::parse(verb.as_str()) else {
            return Vec::new();
        };
        let manifest = self.manifest(repo);
        manifest
            .names(verb)
            .into_iter()
            .map(|name| {
                let takes: Vec<String> = manifest
                    .recipe(verb, &name)
                    .map(|r| r.params.keys().map(|p| format!("--{p}")).collect())
                    .unwrap_or_default();
                let about = match takes.is_empty() {
                    true => String::new(),
                    false => format!("takes {}", takes.join(" ")),
                };
                Candidate::new(&name, &about)
            })
            .collect()
    }

    /// The values a recipe's `--<name>` may take, when it names them.
    fn choices(&self, verb: RecipeVerb, repo: &str, recipe: &str, param: &str) -> Vec<Candidate> {
        let Some(verb) = Verb::parse(verb.as_str()) else {
            return Vec::new();
        };
        let manifest = self.manifest(repo);
        let Some(param) = manifest
            .recipe(verb, recipe)
            .and_then(|r| r.params.get(param))
        else {
            return Vec::new();
        };
        param
            .choices
            .iter()
            .map(|choice| Candidate::new(choice, ""))
            .collect()
    }

    /// The `--<name>` a recipe declares, with its default or that it is required.
    fn params(&self, verb: RecipeVerb, repo: &str, recipe: Option<&str>) -> Vec<Candidate> {
        let (Some(verb), Some(recipe)) = (Verb::parse(verb.as_str()), recipe) else {
            return Vec::new();
        };
        let manifest = self.manifest(repo);
        let Some(recipe) = manifest.recipe(verb, recipe) else {
            return Vec::new();
        };
        recipe
            .params
            .iter()
            .map(|(name, param)| {
                let about = match &param.default {
                    Some(default) => format!("default {default}"),
                    None => "required".to_string(),
                };
                Candidate::new(&format!("--{name}"), &about)
            })
            .collect()
    }

    /// The repo's recipes from both layers, or from this computer's alone when it has no
    /// checkout here.
    fn manifest(&self, repo: &str) -> Manifest {
        let (dir, identity) = match self.checkout(repo) {
            Some(dir) => {
                let identity = Repo(&dir).identity();
                (dir, identity)
            }
            None => (PathBuf::from("/nonexistent"), repo.to_string()),
        };
        Manifest::load_any(Path::new(&dir), &identity).unwrap_or_default()
    }

    fn checkout(&self, repo: &str) -> Option<PathBuf> {
        self.checkouts.as_ref()?.find(repo).ok()
    }
}

impl Candidate {
    pub fn new(value: &str, about: &str) -> Candidate {
        Candidate {
            value: value.to_string(),
            about: about.to_string(),
        }
    }
}

fn flags(flags: &[Flag]) -> Vec<Candidate> {
    flags
        .iter()
        .flat_map(|f| f.names.iter().map(|n| Candidate::new(n, f.about)))
        .collect()
}

/// What a machine is, in a few words: how it is reached, and what limits it.
fn described(machine: &Machine) -> String {
    let mut said = vec![machine.ssh.clone().unwrap_or_default()];
    if !machine.measure {
        said.push("does not measure".to_string());
    }
    if let Some(expires) = machine.expires {
        said.push(format!("leased until {}", Moment::at(expires).minute()));
    }
    said.retain(|s| !s.is_empty());
    said.join(", ")
}
