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
    /// What hardware this needs, in the vocabulary cubecl reports and Slurm consumes.
    #[serde(default)]
    pub needs: Option<String>,
    #[serde(default)]
    pub isolation: Isolation,
    #[serde(default)]
    pub params: BTreeMap<String, Param>,
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
    /// Compiled into the binary. Anyone who has dibs installed has these, with no setup and nothing
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
        Manifest::load_from(dir, repo, &local_dir())
    }

    /// The layer that is normally `local_dir()`, passed in rather than read from the
    /// environment. Tests used to point `DIBS_RECIPES` at a fixture, and cargo runs tests as
    /// threads of one process, so that set a variable other tests were reading at the same
    /// time and this one failed about one run in five. A comment claiming the test was single
    /// threaded is what kept it that way.
    pub fn load_from(dir: &Path, repo: &str, local_dir: &Path) -> Result<Manifest, String> {
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

        let local = local_dir.join(format!("{repo}.toml"));
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
        for (name, mut svc) in other.service {
            svc.source = src;
            self.service.insert(name, svc);
        }
    }

    pub fn service(&self, name: &str) -> Option<&Service> {
        self.service.get(name)
    }

    pub fn service_listing(&self) -> Vec<(&str, Source)> {
        self.service.iter().map(|(k, v)| (k.as_str(), v.source)).collect()
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
    ///
    /// Taken after the parameters are bound, so two values of one knob fingerprint apart: what
    /// ran is what has to be identified, not the template it came from.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.needs.as_deref().unwrap_or("").as_bytes());
        h.update(format!("{:?}", self.isolation).as_bytes());
        for s in &self.steps {
            h.update(format!("{:?}", s.lock).as_bytes());
            h.update(s.run.as_bytes());
            for (k, v) in &s.env {
                h.update(k.as_bytes());
                h.update(v.as_bytes());
            }
        }
        format!("{:x}", h.finalize())[..16].to_string()
    }

    /// The value of every parameter for one invocation: what was asked for, checked against
    /// what is declared, over the defaults.
    pub fn values(&self, given: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
        if let Some(unknown) = given.keys().find(|k| !self.params.contains_key(*k)) {
            let have: Vec<&str> = self.params.keys().map(|s| s.as_str()).collect();
            return Err(match have.is_empty() {
                true => format!("this recipe takes no parameters, so --{unknown} means nothing to it"),
                false => format!("no parameter '{unknown}'; this recipe takes: {}", have.join(", ")),
            });
        }
        let mut out = BTreeMap::new();
        for (name, p) in &self.params {
            let v = match given.get(name).or(p.default.as_ref()) {
                Some(v) => v.clone(),
                None => return Err(format!("--{name} has no default, so it has to be given")),
            };
            if !p.choices.is_empty() && !p.choices.contains(&v) {
                return Err(format!("--{name} {v} is not one of: {}", p.choices.join(", ")));
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
        rec
    }

    /// The two ways a recipe invalidates its own measurement, refused before anything is paid
    /// for rather than found in the numbers afterwards.
    pub fn check(&self, name: &str) -> Result<(), String> {
        for st in &self.steps {
            // CARGO_TARGET_DIR is redirected per tree, so a relative target/ names a directory
            // the build never writes: the step reads whatever an earlier tree left there.
            if let Some(t) = st.run.split_whitespace().find(|w| {
                let w = w.trim_start_matches("./").trim_start_matches(['"', '\'']);
                w == "target" || w.starts_with("target/")
            }) {
                return Err(format!(
                    "recipe '{name}' names {t}, but the build writes to $CARGO_TARGET_DIR, which dibs\n             \
                     puts outside the tree. Use $CARGO_TARGET_DIR/... instead."
                ));
            }
        }
        let cargo = |st: &Step| st.run.split_whitespace().any(|w| w == "cargo");
        let built_first = self.steps.iter().take_while(|st| st.lock == Lock::Shared).any(cargo);
        if let Some(st) = self.steps.iter().find(|st| st.lock == Lock::Exclusive && cargo(st)) {
            if !built_first {
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
        }
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    // The bundled files ship inside the binary, so a typo in one is not a config file the user
    // can fix: it is a build that parses nothing for that repo.
    //
    // And the footgun the cubek header describes is checkable. cubecl's build script counts the
    // enabled runtimes and silently falls back to wgpu when the count is not exactly one, so a
    // recipe against those packages that names two backends, or none, measures hardware nobody
    // chose. Packages that carry their own backend, like cubecl-cuda, select it by package and
    // are left alone.
    #[test]
    fn every_bundled_recipe_parses_and_names_one_backend() {
        const BACKENDS: [&str; 6] = ["cpu", "cuda", "hip", "metal-native", "wgpu", "vulkan"];
        for repo in ["cubek", "cubecl"] {
            let text = super::builtin(repo).expect("a bundled file for this repo");
            let m: super::Manifest =
                toml::from_str(text).unwrap_or_else(|e| panic!("{repo}: {e}"));
            for (verb, named) in [("bench", &m.bench), ("build", &m.build), ("test", &m.test)] {
                for (name, rec) in named {
                    for step in &rec.steps {
                        if !step.run.contains("-p benchmarks") && !step.run.contains("-p throughput")
                        {
                            continue;
                        }
                        let n = BACKENDS
                            .iter()
                            .filter(|b| {
                                step.run.contains(&format!("--features {b}"))
                                    || step.run.contains(&format!("--features cubecl/{b}"))
                            })
                            .count();
                        assert_eq!(n, 1, "{repo} {verb}.{name} names {n} backends: {}", step.run);
                    }
                }
            }
        }
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
        let m = Manifest::load_from(&repo, "cubek", &cfg).unwrap();
        let r = m.recipe(Verb::Build, "x").unwrap();
        assert_eq!(r.steps[0].run, "from local");
        assert_eq!(r.source, Source::Local, "an override has to be visible as one");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_service_carries_its_servers_ports_and_what_makes_them_ready() {
        let tmp = std::env::temp_dir().join(format!("dibs-service-{}", std::process::id()));
        let repo = tmp.join("app");
        let cfg = tmp.join("cfg");
        write(
            &repo,
            ".dibs.toml",
            "[service.gpus]\nbuild=\"cargo build -p server\"\nports=[\"cuda\",\"vulkan\"]\n\
             [[service.gpus.serve]]\nname=\"cuda\"\nrun=\"server --listen :$DIBS_PORT_CUDA\"\nready=\"tcp:cuda\"\n\
             [[service.gpus.serve]]\nname=\"vulkan\"\nrun=\"server --vulkan\"\n",
        );
        let m = Manifest::load_from(&repo, "app", &cfg).unwrap();
        let svc = m.service("gpus").unwrap();
        assert_eq!(svc.build.as_deref(), Some("cargo build -p server"));
        assert_eq!(svc.ports, ["cuda", "vulkan"]);
        assert_eq!(svc.serves.len(), 2);
        assert_eq!((svc.serves[0].name.as_str(), svc.serves[0].ready.as_deref()), ("cuda", Some("tcp:cuda")));
        assert_eq!(svc.serves[1].ready, None, "a server may say nothing about being ready");
        assert_eq!(svc.source, Source::Repo);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn parse(body: &str) -> Recipe {
        let m: Manifest = toml::from_str(body).unwrap();
        m.bench.into_iter().next().unwrap().1
    }

    const SWEEP: &str = "\
[bench.r.params]\n\
backend = { choices = [\"cuda\", \"vulkan\"], default = \"cuda\" }\n\
samples = { default = \"10\" }\n\
size = {}\n\
[[bench.r.step]]\n\
lock = \"shared\"\n\
run = \"cargo build --features cubecl/{backend}\"\n\
[[bench.r.step]]\n\
lock = \"exclusive\"\n\
env = { SAMPLES = \"{samples}\", SHAPE = \"{size}x{size}\" }\n\
run = \"cargo bench --features cubecl/{backend} -- $FILTER\"\n";

    #[test]
    fn a_parameter_falls_back_to_its_default_and_is_checked_against_its_choices() {
        let r = parse(SWEEP);
        let given = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let v = r.values(&given(&[("size", "64")])).unwrap();
        assert_eq!(v["backend"], "cuda");
        assert_eq!(v["samples"], "10");

        let e = r.values(&given(&[("backend", "metal"), ("size", "64")])).unwrap_err();
        assert!(e.contains("cuda, vulkan"), "a refusal has to say what is allowed: {e}");
        let e = r.values(&given(&[("backends", "cuda"), ("size", "64")])).unwrap_err();
        assert!(e.contains("backend, samples, size"), "and which names exist: {e}");
        let e = r.values(&given(&[])).unwrap_err();
        assert!(e.contains("--size"), "a parameter with no default cannot be left out: {e}");
    }

    #[test]
    fn binding_fills_the_declared_names_and_leaves_the_shell_alone() {
        let r = parse(SWEEP);
        let given: BTreeMap<String, String> =
            [("backend", "vulkan"), ("size", "64")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let b = r.bound(&r.values(&given).unwrap());
        assert_eq!(b.steps[0].run, "cargo build --features cubecl/vulkan");
        assert_eq!(b.steps[1].env["SAMPLES"], "10");
        assert_eq!(b.steps[1].env["SHAPE"], "64x64", "a name can appear twice in one value");
        assert!(b.steps[1].run.ends_with("-- $FILTER"), "the command is shell, not a template");
        assert_ne!(r.fingerprint(), b.fingerprint(), "what ran is what has to be identified");
    }

    #[test]
    fn a_step_cannot_name_the_target_directory_it_does_not_write_to() {
        let r = parse(
            "[[bench.r.step]]\nlock=\"shared\"\nrun=\"ls target/release/bench\"\n",
        );
        let e = r.check("r").unwrap_err();
        assert!(e.contains("CARGO_TARGET_DIR"), "{e}");
        let fine = parse(
            "[[bench.r.step]]\nlock=\"shared\"\nrun=\"ls $CARGO_TARGET_DIR/release && ls $R/target-main\"\n",
        );
        fine.check("r").expect("an absolute target directory is the whole point");
    }

    #[test]
    fn a_measurement_that_would_compile_under_the_exclusive_lock_is_refused() {
        let alone = parse("[[bench.r.step]]\nlock=\"exclusive\"\nrun=\"cargo bench --bench gemm\"\n");
        let e = alone.check("r").unwrap_err();
        assert!(e.contains("--no-run"), "the message has to show the two-step form: {e}");
        let split = parse(
            "[[bench.r.step]]\nlock=\"shared\"\nrun=\"cargo bench --no-run\"\n\
             [[bench.r.step]]\nlock=\"exclusive\"\nrun=\"cargo bench --bench gemm\"\n",
        );
        split.check("r").expect("built shared then measured exclusive is the shape being asked for");
        let prebuilt = parse(
            "[[bench.r.step]]\nlock=\"exclusive\"\nrun=\"$CARGO_TARGET_DIR/release/bench\"\n",
        );
        prebuilt.check("r").expect("a binary that was already built compiles nothing");
    }

    #[test]
    fn a_repo_with_no_recipes_anywhere_says_where_it_looked() {
        let tmp = std::env::temp_dir().join(format!("dibs-none-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let e = Manifest::load_from(&tmp, "nothing", &tmp.join("empty")).unwrap_err();
        assert!(e.contains("nothing in"), "an error has to say where it looked: {e}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
