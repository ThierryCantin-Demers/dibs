//! Building one repo against another repo's tree, unpushed changes included.
//!
//! These repos take each other by git revision, so a change to cubecl reaches cubek only once it
//! is pushed and the revision bumped. A pin sends the other tree too and points cargo at it with a
//! `[patch]`, in a config file above the tree rather than in it: the tree stays what was sent.
//! A pinned build still gets a tree of its own, since resolving the patch rewrites the lockfile.

use super::{
    error::PinError,
    local::{Checkout, Fetched, Local, Repo},
    refs::Arm,
    trees::lockfile,
};
use crate::{
    cli::RecipeCall,
    git::{Git, GitError},
    recipe::Checkouts,
};
use dibs_format::lockfile::Package;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// The crates a tree defines: name, and the directory of its manifest relative to the root.
fn crates<'a>(manifests: impl Iterator<Item = (&'a str, String)>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (path, text) in manifests {
        let Ok(v) = toml::from_str::<toml::Value>(&text) else {
            continue;
        };
        if let Some(name) = v
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
        {
            let dir = path
                .strip_suffix("Cargo.toml")
                .unwrap_or(path)
                .trim_end_matches('/');
            out.insert(name.to_string(), dir.to_string());
        }
    }
    out
}

/// A local tree's crates, from the files a send would carry.
fn local_crates(dir: &Path) -> Result<BTreeMap<String, String>, GitError> {
    let list = Git(dir).run(&[
        "ls-files",
        "-co",
        "--exclude-standard",
        "-z",
        "--",
        "Cargo.toml",
        "*/Cargo.toml",
    ])?;
    let paths: Vec<&str> = list.split('\0').filter(|p| !p.is_empty()).collect();
    Ok(crates(paths.iter().filter_map(|p| {
        std::fs::read_to_string(dir.join(p)).ok().map(|t| (*p, t))
    })))
}

/// A ref's crates, read from the repo's history here.
fn ref_crates(dir: &Path, reference: &str) -> Result<BTreeMap<String, String>, GitError> {
    let list = Git(dir).run(&["ls-tree", "-r", "--name-only", "-z", reference])?;
    let paths: Vec<&str> = list
        .split('\0')
        .filter(|p| *p == "Cargo.toml" || p.ends_with("/Cargo.toml"))
        .collect();
    Ok(crates(paths.iter().filter_map(|p| {
        Git(dir)
            .run(&["show", &format!("{reference}:{p}")])
            .ok()
            .map(|t| (*p, t))
    })))
}

/// Where a lockfile takes each of `names` from, as the key a `[patch]` table names the source by.
/// A crate taken from a path is already local and has nothing to patch.
pub fn sources(
    lock: &str,
    names: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, BTreeSet<String>>, PinError> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut take = |name: Option<String>, source: Option<String>| -> Result<(), PinError> {
        let (Some(n), Some(s)) = (name, source) else {
            return Ok(());
        };
        if !names.contains_key(&n) {
            return Ok(());
        }
        let key = if let Some(git) = s.strip_prefix("git+") {
            git.split(['?', '#']).next().unwrap_or(git).to_string()
        } else if s.contains("crates.io-index") || s.starts_with("sparse+https://index.crates.io") {
            "crates-io".to_string()
        } else {
            return Err(PinError::Unpatchable { name: n, source: s });
        };
        out.entry(key).or_default().insert(n);
        Ok(())
    };
    for package in Package::all(lock) {
        take(Some(package.name), package.source)?;
    }
    Ok(out)
}

/// A pinned tree as the machine holds it: where it was prepared, the crates in it by directory,
/// and which of them the lockfile takes from each source.
pub struct PinnedTree {
    pub worktree: String,
    pub crates: BTreeMap<String, String>,
    pub sources: BTreeMap<String, BTreeSet<String>>,
}

/// The config that points each patched crate at its directory in a tree on the machine.
pub fn config(pins: &[PinnedTree]) -> String {
    let mut tables: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for PinnedTree {
        worktree: tree,
        crates,
        sources,
    } in pins
    {
        for (key, names) in sources {
            for n in names {
                let dir = crates.get(n).map(String::as_str).unwrap_or_default();
                let path = if dir.is_empty() {
                    tree.clone()
                } else {
                    format!("{tree}/{dir}")
                };
                tables
                    .entry(key)
                    .or_default()
                    .push(format!("{n} = {{ path = \"{path}\" }}"));
            }
        }
    }
    let mut s = String::new();
    for (key, lines) in tables {
        let header = if key == "crates-io" {
            "[patch.crates-io]".to_string()
        } else {
            format!("[patch.\"{key}\"]")
        };
        s += &format!("{header}\n{}\n", lines.join("\n"));
    }
    s
}

/// `--pin <repo>@<ref>`, both halves named.
pub struct PinSpec<'a> {
    pub repo: &'a str,
    pub reference: &'a str,
}

pub fn pin_spec(p: &str) -> Result<PinSpec<'_>, PinError> {
    match p.split_once('@') {
        Some((repo, reference))
            if !repo.is_empty()
                && !reference.is_empty()
                && !reference.contains("..")
                && !reference.contains(',') =>
        {
            Ok(PinSpec { repo, reference })
        }
        _ => Err(PinError::Malformed(p.to_string())),
    }
}

/// A repo built against in place of what the lockfile names, and what that replaces.
pub struct Pinned {
    pub repo: String,
    pub reference: String,
    pub dir: PathBuf,
    /// The tree sent, when one is: the one at `dir`, or a checkout of a commit the machine cannot fetch.
    pub local: Option<super::Local>,
    pub checkout: Option<super::Checkout>,
    pub note: Option<String>,
    pub crates: BTreeMap<String, String>,
    pub lock: Option<String>,
    /// The sources a `[patch]` has to redirect, and the crates taken from each.
    pub sources: BTreeMap<String, std::collections::BTreeSet<String>>,
}

/// Every pin, resolved against every arm's lockfile and every pin's: one pinned crate may reach
/// the build through another pinned repo rather than through this one.
pub fn pins_of(
    args: &RecipeCall,
    repo: &str,
    dir: &Path,
    arms: &[Arm],
) -> Result<Vec<Pinned>, PinError> {
    let mut pins = Vec::new();
    for p in &args.pins {
        let PinSpec {
            repo: name,
            reference,
        } = pin_spec(p)?;
        let pdir = Checkouts::of(args)?.find(name)?;
        let identity = Repo(&pdir).identity();
        if identity == repo {
            return Err(PinError::Itself {
                pin: p.clone(),
                repo: repo.to_string(),
            });
        }
        if pins.iter().any(|q: &Pinned| q.repo == identity) {
            return Err(PinError::Twice {
                pin: p.clone(),
                repo: identity,
            });
        }
        let (local, checkout, note, crates, lock) = match reference {
            "local" => (
                Some(Local::of(&pdir)?),
                None,
                None,
                local_crates(&pdir)?,
                lockfile(&pdir, None),
            ),
            _ => {
                let Fetched {
                    commit: sha,
                    seen,
                    ahead,
                } = Fetched::of(&pdir, reference).ok_or_else(|| PinError::NoRef {
                    pin: p.clone(),
                    reference: reference.to_string(),
                    dir: pdir.clone(),
                })?;
                let (crates, lock) = (ref_crates(&pdir, &sha)?, lockfile(&pdir, Some(&sha)));
                match ahead.or_else(|| Repo(&pdir).unfetchable(&sha)) {
                    Some(why) => {
                        let c = Checkout::of(&pdir, &identity, &sha, Some(why))?;
                        (
                            Some(c.local()?),
                            Some(c),
                            Some(format!("as {seen} stands here")),
                            crates,
                            lock,
                        )
                    }
                    None => (None, None, None, crates, lock),
                }
            }
        };
        pins.push(Pinned {
            repo: identity,
            reference: reference.to_string(),
            dir: pdir,
            local,
            checkout,
            note,
            crates,
            lock,
            sources: BTreeMap::new(),
        });
    }
    let locks: Vec<String> = arms
        .iter()
        .filter_map(|a| lockfile(a.dir(dir), a.fetch.as_deref()))
        .chain(pins.iter().filter_map(|p| p.lock.clone()))
        .collect();
    for p in &mut pins {
        for lock in &locks {
            for (source, names) in sources(lock, &p.crates)? {
                p.sources.entry(source).or_default().extend(names);
            }
        }
        if p.sources.is_empty() {
            return Err(PinError::NothingToReplace {
                pinned: p.repo.clone(),
                repo: repo.to_string(),
            });
        }
    }
    Ok(pins)
}

#[cfg(test)]
mod tests {

    use super::*;

    const LOCK: &str = r#"
[[package]]
name = "cubecl"
version = "0.11.0"
source = "git+https://github.com/tracel-ai/cubecl?rev=d005#d0052727"

[[package]]
name = "cubecl-core"
version = "0.11.0"
source = "git+https://github.com/tracel-ai/cubecl?rev=d005#d0052727"

[[package]]
name = "cubek"
version = "0.3.0"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

    fn cubecl() -> BTreeMap<String, String> {
        crates(
            [
                (
                    "Cargo.toml",
                    "[workspace]\nmembers = [\"crates/*\"]\n".to_string(),
                ),
                (
                    "crates/cubecl/Cargo.toml",
                    "[package]\nname = \"cubecl\"\nversion.workspace = true\n".to_string(),
                ),
                (
                    "crates/cubecl-core/Cargo.toml",
                    "[package]\nname = \"cubecl-core\"\n".to_string(),
                ),
                (
                    "crates/cubecl-cpp/Cargo.toml",
                    "[package]\nname = \"cubecl-cpp\"\n".to_string(),
                ),
            ]
            .iter()
            .map(|(p, t)| (*p, t.clone())),
        )
    }

    #[test]
    fn a_tree_s_crates_are_its_packages_not_its_workspace() {
        let c = cubecl();
        assert_eq!(c.len(), 3);
        assert_eq!(c["cubecl-core"], "crates/cubecl-core");
    }

    #[test]
    fn only_what_the_lock_takes_from_elsewhere_is_patched_and_by_its_source() {
        let s = sources(LOCK, &cubecl()).unwrap();
        assert_eq!(s.len(), 1);
        let names: Vec<&str> = s["https://github.com/tracel-ai/cubecl"]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            names,
            ["cubecl", "cubecl-core"],
            "cubecl-cpp is not in the graph, so a patch for it would only warn"
        );
        let text = config(&[PinnedTree {
            worktree: "/m/ws/cubecl/local-k".into(),
            crates: cubecl(),
            sources: s,
        }]);
        assert_eq!(
            text,
            "[patch.\"https://github.com/tracel-ai/cubecl\"]\ncubecl = { path = \"/m/ws/cubecl/local-k/crates/cubecl\" }\ncubecl-core = { path = \"/m/ws/cubecl/local-k/crates/cubecl-core\" }\n"
        );
        assert!(toml::from_str::<toml::Value>(&text).is_ok());
    }

    #[test]
    fn a_crate_from_the_registry_is_patched_as_crates_io() {
        let serde = crates(
            [("Cargo.toml", "[package]\nname = \"serde\"\n".to_string())]
                .iter()
                .map(|(p, t)| (*p, t.clone())),
        );
        let s = sources(LOCK, &serde).unwrap();
        assert!(
            config(&[PinnedTree {
                worktree: "/t".into(),
                crates: serde,
                sources: s
            }])
            .starts_with("[patch.crates-io]\nserde = { path = \"/t\" }")
        );
    }
}
