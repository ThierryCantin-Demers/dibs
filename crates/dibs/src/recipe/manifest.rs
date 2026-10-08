//! `.dibs.toml`, which lives in the repo being measured rather than here.
//!
//! The repo knows its own build and benchmark commands, they version with the code, and a
//! benchmark added in a pull request brings its recipe with it. That is also what makes a run
//! reproducible: check out the ref, read the recipe, run it again.
//!
//! A recipe declares a procedure and names no revisions. Pinning them here would bind the
//! procedure to a moment and make it progressively harder to rerun, which is the opposite of
//! what putting it in the repo was for. The revisions belong to the run record.

use super::{
    base::Recipe,
    error::{ManifestError, RecipeError},
};
use crate::paths::Paths;
use dibs_format::RunVerb;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// One server, and what it takes for something to be able to use it.
#[derive(Debug, Deserialize, Clone)]
pub struct Serve {
    pub name: String,
    pub run: String,
    /// tcp:<port or port name>, or a command that exits 0 once the server answers. Without it a
    /// client races the server it was started for.
    #[serde(default)]
    pub ready: Option<String>,
}

/// Servers a repo knows how to start, for work that runs against them rather than in them. Not a
/// recipe: it measures nothing itself, and it lives exactly as long as the command using it.
#[derive(Debug, Deserialize, Clone)]
pub struct Service {
    #[serde(skip)]
    pub source: Source,
    /// Run under the shared lock before anything is served, since a server must never compile:
    /// it would do so inside whatever lock the command using it holds.
    #[serde(default)]
    pub build: Option<String>,
    /// Named rather than numbered, so the machine picks one nothing is listening on and both
    /// sides read it from the environment.
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default, rename = "serve")]
    pub serves: Vec<Serve>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    #[serde(default)]
    pub bench: BTreeMap<String, Recipe>,
    #[serde(default)]
    pub build: BTreeMap<String, Recipe>,
    #[serde(default)]
    pub test: BTreeMap<String, Recipe>,
    #[serde(default)]
    pub service: BTreeMap<String, Service>,
    #[serde(default)]
    pub tree: Option<Tree>,
}

/// How the repo's trees are set up on a machine, whichever recipe then runs in them.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct Tree {
    /// Paths a new tree starts without when it is seeded from a sibling, such as a cache a tool
    /// keeps inside the tree, whose contents would be the sibling's results rather than its own.
    #[serde(default)]
    pub fresh: Vec<String>,
}

impl Tree {
    /// Each is removed with `rm -rf` inside the new tree, so it has to name something in it.
    fn check(&self) -> Result<(), ManifestError> {
        for p in &self.fresh {
            let inside = p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c));
            if !inside {
                return Err(ManifestError::Fresh(p.clone()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Verb {
    Bench,
    Build,
    Test,
}

impl Verb {
    pub fn parse(s: &str) -> Option<Verb> {
        match s {
            "bench" => Some(Verb::Bench),
            "build" => Some(Verb::Build),
            "test" => Some(Verb::Test),
            _ => None,
        }
    }
    /// What a record of a run of this verb says was run.
    pub fn run_verb(self) -> RunVerb {
        match self {
            Verb::Bench => RunVerb::Bench,
            Verb::Build => RunVerb::Build,
            Verb::Test => RunVerb::Test,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Verb::Bench => "bench",
            Verb::Build => "build",
            Verb::Test => "test",
        }
    }
}

/// Where a recipe was found, so an override is visible rather than surprising.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// `.dibs.toml` in the repo being measured, for a repo that wants to carry its own. Not
    /// a destination recipes graduate to: a shared upstream repo gains nothing from one
    /// person's benchmark procedure, and the run record already carries the procedure itself.
    #[default]
    Repo,
    /// `~/.config/dibs/recipes/<repo>.toml`, which can be a clone a team shares. A recipe in a
    /// shared upstream repo costs a pull request per change and gives every other contributor
    /// something they do not use.
    Local,
}

/// A recipe or a service by name, and the file it came from, as `dibs list` shows it.
pub struct Listed<'a> {
    pub name: &'a str,
    pub source: Source,
}

impl Manifest {
    /// This computer's recipes, which override a repo's own.
    pub fn local_dir() -> PathBuf {
        Paths::from_env()
            .recipes()
            .unwrap_or_else(|| PathBuf::from("dibs/recipes"))
    }

    /// Every repo with a recipes file on this computer.
    pub fn local_repos() -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(Manifest::local_dir()) else {
            return Vec::new();
        };
        let mut repos: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_suffix(".toml")
                    .map(str::to_string)
            })
            .collect();
        repos.sort();
        repos
    }

    /// Two layers, the second overriding the first: whatever the repo declares for itself, then
    /// local config, because that is the override. The format is the same in both, so a recipe
    /// moves between them unchanged. dibs carries no recipes of its own: it knows no repo.
    pub fn load(dir: &Path, repo: &str) -> Result<Manifest, RecipeError> {
        Manifest::load_from(dir, repo, &Manifest::local_dir())
    }

    /// The layer that is normally `Manifest::local_dir()`, passed in: a variable set for one test is read
    /// by every other test thread of the process.
    pub fn load_from(dir: &Path, repo: &str, local_dir: &Path) -> Result<Manifest, RecipeError> {
        Manifest::layers(dir, repo, local_dir)?.ok_or_else(|| RecipeError::NoRecipes {
            repo: repo.to_string(),
            in_repo: dir.join(".dibs.toml"),
            local: local_dir.join(format!("{repo}.toml")),
        })
    }

    /// For work in a tree that runs no recipe, where a repo declaring nothing is not an error.
    pub fn load_any(dir: &Path, repo: &str) -> Result<Manifest, RecipeError> {
        Ok(Manifest::layers(dir, repo, &Manifest::local_dir())?.unwrap_or_default())
    }

    /// None when neither layer has a file.
    fn layers(dir: &Path, repo: &str, local_dir: &Path) -> Result<Option<Manifest>, RecipeError> {
        let mut found: Option<Manifest> = None;
        for (path, src) in [
            (dir.join(".dibs.toml"), Source::Repo),
            (local_dir.join(format!("{repo}.toml")), Source::Local),
        ] {
            if !path.exists() {
                continue;
            }
            let at = |why: ManifestError| RecipeError::File {
                path: path.clone(),
                why,
            };
            let text = std::fs::read_to_string(&path).map_err(|e| at(ManifestError::Read(e)))?;
            let parsed: Manifest =
                toml::from_str(&text).map_err(|e| at(ManifestError::Parse(Box::new(e))))?;
            parsed.refuse_needs().map_err(at)?;
            parsed
                .tree
                .as_ref()
                .map(Tree::check)
                .transpose()
                .map_err(at)?;
            found
                .get_or_insert_with(Manifest::default)
                .absorb(parsed, src);
        }
        Ok(found)
    }

    fn refuse_needs(&self) -> Result<(), ManifestError> {
        let named = [&self.bench, &self.build, &self.test]
            .into_iter()
            .flatten()
            .find_map(|(name, rec)| Some((name, rec.needs.as_deref()?)));
        match named {
            Some((name, needs)) => Err(ManifestError::Needs {
                recipe: name.clone(),
                needs: needs.to_string(),
            }),
            None => Ok(()),
        }
    }

    /// What a new tree of this repo starts without, from whichever layer last said.
    pub fn tree_fresh(&self) -> &[String] {
        self.tree.as_ref().map_or(&[], |t| &t.fresh)
    }

    fn absorb(&mut self, other: Manifest, src: Source) {
        for (table, incoming) in [
            (&mut self.bench, other.bench),
            (&mut self.build, other.build),
            (&mut self.test, other.test),
        ] {
            for (name, mut rec) in incoming {
                rec.source = src;
                table.insert(name, rec);
            }
        }
        for (name, mut svc) in other.service {
            svc.source = src;
            self.service.insert(name, svc);
        }
        if other.tree.is_some() {
            self.tree = other.tree;
        }
    }

    pub fn service(&self, name: &str) -> Option<&Service> {
        self.service.get(name)
    }

    pub fn service_listing(&self) -> Vec<Listed<'_>> {
        self.service
            .iter()
            .map(|(name, v)| Listed {
                name,
                source: v.source,
            })
            .collect()
    }

    pub fn recipe(&self, verb: Verb, name: &str) -> Option<&Recipe> {
        let table = match verb {
            Verb::Bench => &self.bench,
            Verb::Build => &self.build,
            Verb::Test => &self.test,
        };
        table.get(name)
    }

    /// Every recipe this file defines, for saying what is available when a name is wrong.
    pub fn names(&self, verb: Verb) -> Vec<String> {
        self.table(verb).keys().cloned().collect()
    }

    pub fn listing(&self, verb: Verb) -> Vec<Listed<'_>> {
        self.table(verb)
            .iter()
            .map(|(name, v)| Listed {
                name,
                source: v.source,
            })
            .collect()
    }

    fn table(&self, verb: Verb) -> &BTreeMap<String, Recipe> {
        match verb {
            Verb::Bench => &self.bench,
            Verb::Build => &self.build,
            Verb::Test => &self.test,
        }
    }
}
