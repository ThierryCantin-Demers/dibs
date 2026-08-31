//! `.dibs.toml`, which lives in the repo being measured rather than here.
//!
//! The repo knows its own build and benchmark commands, they version with the code, and a
//! benchmark added in a pull request brings its recipe with it. That is also what makes a run
//! reproducible: check out the ref, read the recipe, run it again.
//!
//! A recipe declares a procedure and names no revisions. Pinning them here would bind the
//! procedure to a moment and make it progressively harder to rerun, which is the opposite of
//! what putting it in the repo was for. The revisions belong to the run record.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn default_source() -> Source {
    Source::Repo
}

/// Compiled in, so there is nothing to install and nothing to keep in sync. Adding a repo
/// here is a file and a line, and everyone gets it on the next build.
fn builtin(repo: &str) -> Option<&'static str> {
    match repo {
        "cubek" => Some(include_str!("../recipes/cubek.toml")),
        "cubecl" => Some(include_str!("../recipes/cubecl.toml")),
        _ => None,
    }
}

pub fn local_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("DIBS_RECIPES") {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default();
    base.join("dibs/recipes")
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Lock {
    /// Builds, tests, inspection. Several at once.
    Shared,
    /// The measured run. Nothing else.
    Exclusive,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    /// Nothing else runs on the machine. The default, because the failure mode of the other
    /// one is a number that is wrong and looks fine.
    #[default]
    Machine,
    /// This device only; neighbours may use theirs.
    Device,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Step {
    pub lock: Lock,
    pub run: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Recipe {
    /// Filled in on load, never read from the file.
    #[serde(skip, default = "default_source")]
    pub source: Source,
    /// What hardware this needs, in the vocabulary cubecl reports and Slurm consumes.
    #[serde(default)]
    pub needs: Option<String>,
    #[serde(default)]
    pub isolation: Isolation,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    #[serde(default)]
    pub bench: BTreeMap<String, Recipe>,
    #[serde(default)]
    pub build: BTreeMap<String, Recipe>,
    #[serde(default)]
    pub test: BTreeMap<String, Recipe>,
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
    /// Compiled into the binary. Anyone who has dibs-run has these, with no setup and nothing
    /// to sync, which is the only arrangement that works for someone who does not share the
    /// same dotfile manager.
    Builtin,
    /// `.dibs.toml` in the repo being measured, for a repo that wants to carry its own. Not
    /// a destination recipes graduate to: a shared upstream repo gains nothing from one
    /// person's benchmark procedure, and the run record already carries the procedure itself.
    Repo,
    /// `~/.config/dibs/recipes/<repo>.toml`. Where a recipe lives while it is still moving:
    /// these are shared upstream repos, and an experimental file in one costs a pull request
    /// and gives every other contributor something they do not use.
    Local,
}

impl Manifest {
    /// Three layers, each overriding the last: bundled defaults, then whatever the repo
    /// declares for itself, then local config. Defaults so a new person has working recipes
    /// the moment they have the binary; the repo above them because a repo that declares its
    /// own knows better than a default; local above both because it is the override, and it
    /// is what lets a recipe be iterated on without a pull request against a shared upstream
    /// repo. The format is identical at every layer, so a recipe moves down as it settles.
    pub fn load(dir: &Path, repo: &str) -> Result<Manifest, String> {
        let mut m = Manifest::default();
        let mut found = Vec::new();

        if let Some(text) = builtin(repo) {
            let parsed: Manifest =
                toml::from_str(text).map_err(|e| format!("bundled {repo}.toml: {e}"))?;
            m.absorb(parsed, Source::Builtin);
            found.push(format!("bundled {repo}.toml"));
        }

        let in_repo = dir.join(".dibs.toml");
        if in_repo.exists() {
            let text = std::fs::read_to_string(&in_repo)
                .map_err(|e| format!("{}: {e}", in_repo.display()))?;
            let parsed: Manifest =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", in_repo.display()))?;
            m.absorb(parsed, Source::Repo);
            found.push(in_repo.display().to_string());
        }

        let local = local_dir().join(format!("{repo}.toml"));
        if local.exists() {
            let text = std::fs::read_to_string(&local)
                .map_err(|e| format!("{}: {e}", local.display()))?;
            let parsed: Manifest =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", local.display()))?;
            m.absorb(parsed, Source::Local);
            found.push(local.display().to_string());
        }

        if found.is_empty() {
            return Err(format!(
                "no recipes for {repo}. Nothing bundled, and nothing in {} or {}",
                in_repo.display(),
                local.display()
            ));
        }
        Ok(m)
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
        self.table(verb).iter().map(|(k, v)| (k.as_str(), v.source)).collect()
    }

    fn table(&self, verb: Verb) -> &BTreeMap<String, Recipe> {
        match verb {
            Verb::Bench => &self.bench,
            Verb::Build => &self.build,
            Verb::Test => &self.test,
        }
    }
}

impl Recipe {
    /// Identifies the procedure a number was produced by. Recorded with every run, because a
    /// label alone is not provenance: one name can cover two different benchmarks at two refs,
    /// and comparing across that is the failure the history exists to prevent.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.needs.as_deref().unwrap_or("").as_bytes());
        h.update(format!("{:?}", self.isolation).as_bytes());
        for s in &self.steps {
            h.update(format!("{:?}", s.lock).as_bytes());
            h.update(s.run.as_bytes());
        }
        format!("{:x}", h.finalize())[..16].to_string()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    /// Local wins, because it is the override, and because these are shared upstream repos:
    /// a recipe still moving cannot live in one without costing a pull request.
    #[test]
    fn local_config_overrides_the_repo() {
        let tmp = std::env::temp_dir().join(format!("dibs-recipe-{}", std::process::id()));
        let repo = tmp.join("cubek");
        let cfg = tmp.join("cfg");
        write(&repo, ".dibs.toml", "[build.x]\n[[build.x.step]]\nlock=\"shared\"\nrun=\"from repo\"\n");
        write(&cfg, "cubek.toml", "[build.x]\n[[build.x.step]]\nlock=\"shared\"\nrun=\"from local\"\n");
        // SAFETY: single-threaded test, and the variable is read once during load.
        unsafe { std::env::set_var("DIBS_RECIPES", &cfg) };
        let m = Manifest::load(&repo, "cubek").unwrap();
        let r = m.recipe(Verb::Build, "x").unwrap();
        assert_eq!(r.steps[0].run, "from local");
        assert_eq!(r.source, Source::Local, "an override has to be visible as one");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_repo_with_no_recipes_anywhere_says_where_it_looked() {
        let tmp = std::env::temp_dir().join(format!("dibs-none-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("DIBS_RECIPES", tmp.join("empty")) };
        let e = Manifest::load(&tmp, "nothing").unwrap_err();
        assert!(e.contains("nothing in"), "an error has to say where it looked: {e}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
