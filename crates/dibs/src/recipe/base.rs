//! A recipe: the procedure a repo or this computer declares, its parameters, and what makes two
//! runs of it the same procedure.

use super::{
    error::{Flaw, ParamError, RecipeError},
    manifest::Source,
};
use dibs_format::Lock;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

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
    #[serde(skip)]
    pub source: Source,
    /// Refused on load: nothing here can check what a machine has or route on it.
    #[serde(default)]
    pub needs: Option<String>,
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

impl Recipe {
    /// The value of every parameter for one invocation: what was asked for, checked against
    /// what is declared, over the defaults.
    pub fn values(
        &self,
        given: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, ParamError> {
        if let Some(unknown) = given.keys().find(|k| !self.params.contains_key(*k)) {
            return Err(match self.params.is_empty() {
                true => ParamError::TakesNone(unknown.clone()),
                false => ParamError::Unknown {
                    name: unknown.clone(),
                    have: self.params.keys().cloned().collect(),
                },
            });
        }
        let mut out = BTreeMap::new();
        for (name, p) in &self.params {
            let v = match given.get(name).or(p.default.as_ref()) {
                Some(v) => v.clone(),
                None => return Err(ParamError::NoDefault(name.clone())),
            };
            if !p.choices.is_empty() && !p.choices.contains(&v) {
                return Err(ParamError::NotAChoice {
                    name: name.clone(),
                    value: v,
                    choices: p.choices.clone(),
                });
            }
            out.insert(name.clone(), v);
        }
        Ok(out)
    }

    /// The recipe as it will run, with `{name}` replaced in every command and every exported
    /// value. Only declared names are substituted: a command is shell, and `${VAR}`, `awk
    /// '{print $1}'` and `find -exec {}` all pass through untouched.
    pub fn bound(&self, values: &BTreeMap<String, String>) -> Recipe {
        let fill = |s: &str| {
            let mut out = s.to_string();
            for (k, v) in values {
                out = out.replace(&format!("{{{k}}}"), v);
            }
            out
        };
        let mut rec = self.clone();
        for st in &mut rec.steps {
            st.run = fill(&st.run);
            st.env = st.env.iter().map(|(k, v)| (k.clone(), fill(v))).collect();
        }
        rec.artifacts = rec.artifacts.iter().map(|a| fill(a)).collect();
        rec
    }

    /// Identifies the procedure a number was produced by. Recorded with every run, because a
    /// label alone is not provenance: one name can cover two different benchmarks at two refs,
    /// and comparing across that is the failure the history exists to prevent.
    ///
    /// Taken after the parameters are bound, so two values of one knob fingerprint apart: what
    /// ran is what has to be identified, not the template it came from.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(format!("{:?}", self.isolation).as_bytes());
        for s in &self.steps {
            h.update(format!("{:?}", s.lock).as_bytes());
            h.update(s.run.as_bytes());
            for (k, v) in &s.env {
                h.update(k.as_bytes());
                h.update(v.as_bytes());
            }
        }
        for f in &self.fresh {
            h.update(b"fresh");
            h.update(f.as_bytes());
        }
        format!("{:x}", h.finalize())[..16].to_string()
    }

    /// The two ways a recipe invalidates its own measurement, refused before anything is paid
    /// for rather than found in the numbers afterwards.
    pub fn check(&self, name: &str) -> Result<(), RecipeError> {
        let unsound = |flaw: Flaw| RecipeError::Unsound {
            recipe: name.to_string(),
            flaw,
        };
        let variable = |v: &str| {
            v.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        if let Some(v) = self.fresh.iter().find(|v| !variable(v)) {
            return Err(unsound(Flaw::Fresh(v.clone())));
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
            return Err(unsound(Flaw::Artifact(a.clone())));
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
                return Err(unsound(Flaw::Target(t.to_string())));
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
            return Err(unsound(Flaw::CompilesExclusive(st.run.clone())));
        }
        Ok(())
    }
}
