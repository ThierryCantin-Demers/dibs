//! `.dibs.toml`, which lives in the repo being measured rather than here.
//!
//! The repo knows its own build and benchmark commands, they version with the code, and a
//! benchmark added in a pull request brings its recipe with it. That is also what makes a run
//! reproducible: check out the ref, read the recipe, run it again.
//!
//! A recipe declares a procedure and names no revisions. Pinning them here would bind the
//! procedure to a moment and make it progressively harder to rerun, which is the opposite of
//! what putting it in the repo was for. The revisions belong to the run record.

use super::refusals::RecipeError;
use dibs::paths::Paths;
use dibs_format::{Lock, RunVerb};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn default_source() -> Source {
    Source::Repo
}

pub fn local_dir() -> PathBuf {
    Paths::from_env()
        .recipes()
        .unwrap_or_else(|| PathBuf::from("dibs/recipes"))
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// Nothing else runs on the machine, the only isolation there is.
    #[default]
    Machine,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Step {
    pub lock: Lock,
    pub run: String,
    /// Exported before the command, since ssh forwards nothing from this side.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// One knob a recipe takes. Declaring them is what keeps the set of valid invocations
/// enumerable, so `dibs list` can say what a recipe accepts instead of the caller reading it.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct Param {
    #[serde(default)]
    pub default: Option<String>,
    /// When it is not empty, a value outside it is refused rather than passed to the workload,
    /// which is where a typo currently becomes a silently different measurement.
    #[serde(default)]
    pub choices: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Recipe {
    /// Filled in on load, never read from the file.
    #[serde(skip, default = "default_source")]
    pub source: Source,
    /// Refused on load: nothing here can check what a machine has or route on it.
    #[serde(default)]
    pub(crate) needs: Option<String>,
    #[serde(default)]
    pub isolation: Isolation,
    #[serde(default)]
    pub params: BTreeMap<String, Param>,
    /// Variables given a value unique to each run, such as one naming the store a tool keeps
    /// autotune results in, which a run would otherwise share with the last run in its tree. A
    /// value rather than a directory, since that is what a tool's knob takes.
    #[serde(default)]
    pub fresh: Vec<String>,
    /// Files a step writes that the caller wants back, relative to the tree or under
    /// `$CARGO_TARGET_DIR/`. Each step keeps those it wrote itself, never one an earlier run left.
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

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
    #[serde(skip, default = "default_source")]
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
    fn check(&self) -> Result<(), String> {
        for p in &self.fresh {
            let inside = p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c));
            if !inside {
                return Err(format!(
                    "[tree] fresh lists paths inside the tree, and '{p}' is not one: relative, with no . or .. \
                     part, in letters, digits and ._-/"
                ));
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `.dibs.toml` in the repo being measured, for a repo that wants to carry its own. Not
    /// a destination recipes graduate to: a shared upstream repo gains nothing from one
    /// person's benchmark procedure, and the run record already carries the procedure itself.
    Repo,
    /// `~/.config/dibs/recipes/<repo>.toml`, which can be a clone a team shares. A recipe in a
    /// shared upstream repo costs a pull request per change and gives every other contributor
    /// something they do not use.
    Local,
}

impl Manifest {
    /// Two layers, the second overriding the first: whatever the repo declares for itself, then
    /// local config, because that is the override. The format is the same in both, so a recipe
    /// moves between them unchanged. dibs carries no recipes of its own: it knows no repo.
    pub fn load(dir: &Path, repo: &str) -> Result<Manifest, RecipeError> {
        Manifest::load_from(dir, repo, &local_dir())
    }

    /// The layer that is normally `local_dir()`, passed in: a variable set for one test is read
    /// by every other test thread of the process.
    pub fn load_from(dir: &Path, repo: &str, local_dir: &Path) -> Result<Manifest, RecipeError> {
        let (m, found) = Manifest::layers(dir, repo, local_dir)?;
        if !found {
            return Err(RecipeError::NoRecipes {
                repo: repo.to_string(),
                in_repo: dir.join(".dibs.toml"),
                local: local_dir.join(format!("{repo}.toml")),
            });
        }
        Ok(m)
    }

    /// For work in a tree that runs no recipe, where a repo declaring nothing is not an error.
    pub fn load_any(dir: &Path, repo: &str) -> Result<Manifest, RecipeError> {
        Manifest::layers(dir, repo, &local_dir()).map(|(m, _)| m)
    }

    fn layers(dir: &Path, repo: &str, local_dir: &Path) -> Result<(Manifest, bool), RecipeError> {
        let mut m = Manifest::default();
        let mut found = false;
        for (path, src) in [
            (dir.join(".dibs.toml"), Source::Repo),
            (local_dir.join(format!("{repo}.toml")), Source::Local),
        ] {
            if !path.exists() {
                continue;
            }
            let at = |why: String| RecipeError::File {
                path: path.clone(),
                why,
            };
            let text = std::fs::read_to_string(&path).map_err(|e| at(e.to_string()))?;
            let parsed: Manifest = toml::from_str(&text).map_err(|e| at(e.to_string()))?;
            parsed.refuse_needs().map_err(at)?;
            parsed
                .tree
                .as_ref()
                .map(Tree::check)
                .transpose()
                .map_err(at)?;
            m.absorb(parsed, src);
            found = true;
        }
        Ok((m, found))
    }

    fn refuse_needs(&self) -> Result<(), String> {
        let named = [&self.bench, &self.build, &self.test]
            .into_iter()
            .flatten()
            .find_map(|(name, rec)| Some((name, rec.needs.as_deref()?)));
        match named {
            Some((name, needs)) => Err(format!(
                "recipe {name} needs '{needs}', which nothing here can check or route on; name the machine with --on"
            )),
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

    pub fn service_listing(&self) -> Vec<(&str, Source)> {
        self.service
            .iter()
            .map(|(k, v)| (k.as_str(), v.source))
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
    pub fn names(&self, verb: Verb) -> Vec<&str> {
        self.table(verb).keys().map(|s| s.as_str()).collect()
    }

    pub fn listing(&self, verb: Verb) -> Vec<(&str, Source)> {
        self.table(verb)
            .iter()
            .map(|(k, v)| (k.as_str(), v.source))
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
