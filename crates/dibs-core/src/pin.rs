//! Building one repo against another repo's tree, unpushed changes included.
//!
//! These repos take each other by git revision, so a change to cubecl reaches cubek only once it
//! is pushed and the revision bumped. A pin sends the other tree too and points cargo at it with a
//! `[patch]`, in a config file above the tree rather than in it: the tree stays what was sent.
//! A pinned build still gets a tree of its own, since resolving the patch rewrites the lockfile.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The crates a tree defines: name, and the directory of its manifest relative to the root.
pub fn crates<'a>(manifests: impl Iterator<Item = (&'a str, String)>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (path, text) in manifests {
        let Ok(v) = toml::from_str::<toml::Value>(&text) else { continue };
        if let Some(name) = v.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
            let dir = path.strip_suffix("Cargo.toml").unwrap_or(path).trim_end_matches('/');
            out.insert(name.to_string(), dir.to_string());
        }
    }
    out
}

/// A local tree's crates, from the files a send would carry.
pub fn local_crates(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let list = git(dir, &["ls-files", "-co", "--exclude-standard", "-z", "--", "Cargo.toml", "*/Cargo.toml"])?;
    let paths: Vec<&str> = list.split('\0').filter(|p| !p.is_empty()).collect();
    Ok(crates(paths.iter().filter_map(|p| std::fs::read_to_string(dir.join(p)).ok().map(|t| (*p, t)))))
}

/// A ref's crates, read from the repo's history here.
pub fn ref_crates(dir: &Path, reference: &str) -> Result<BTreeMap<String, String>, String> {
    let list = git(dir, &["ls-tree", "-r", "--name-only", "-z", reference])?;
    let paths: Vec<&str> = list.split('\0').filter(|p| *p == "Cargo.toml" || p.ends_with("/Cargo.toml")).collect();
    Ok(crates(paths.iter().filter_map(|p| git(dir, &["show", &format!("{reference}:{p}")]).ok().map(|t| (*p, t)))))
}

/// Where a lockfile takes each of `names` from, as the key a `[patch]` table names the source by.
/// A crate taken from a path is already local and has nothing to patch.
pub fn sources(lock: &str, names: &BTreeMap<String, String>) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut take = |name: Option<String>, source: Option<String>| -> Result<(), String> {
        let (Some(n), Some(s)) = (name, source) else { return Ok(()) };
        if !names.contains_key(&n) {
            return Ok(());
        }
        let key = if let Some(git) = s.strip_prefix("git+") {
            git.split(['?', '#']).next().unwrap_or(git).to_string()
        } else if s.contains("crates.io-index") || s.starts_with("sparse+https://index.crates.io") {
            "crates-io".to_string()
        } else {
            return Err(format!("{n} comes from {s}, which a pin does not know how to replace"));
        };
        out.entry(key).or_default().insert(n);
        Ok(())
    };
    let (mut name, mut source) = (None, None);
    for line in lock.lines().map(str::trim) {
        if line == "[[package]]" {
            take(name.take(), source.take())?;
        } else if let Some(v) = line.strip_prefix("name = ") {
            name = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("source = ") {
            source = Some(v.trim_matches('"').to_string());
        }
    }
    take(name, source)?;
    Ok(out)
}

/// The config that points each patched crate at its directory in a tree on the machine.
pub fn config(pins: &[(String, BTreeMap<String, String>, BTreeMap<String, BTreeSet<String>>)]) -> String {
    let mut tables: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (tree, crates, sources) in pins {
        for (key, names) in sources {
            for n in names {
                let dir = crates.get(n).map(String::as_str).unwrap_or_default();
                let path = if dir.is_empty() { tree.clone() } else { format!("{tree}/{dir}") };
                tables.entry(key).or_default().push(format!("{n} = {{ path = \"{path}\" }}"));
            }
        }
    }
    let mut s = String::new();
    for (key, lines) in tables {
        let header = if key == "crates-io" { "[patch.crates-io]".to_string() } else { format!("[patch.\"{key}\"]") };
        s += &format!("{header}\n{}\n", lines.join("\n"));
    }
    s
}

/// A build step that fails when cargo still takes a patched crate from where it took it before,
/// which is what a pin whose versions do not satisfy the requirement looks like: a build that
/// succeeds against the published code.
pub fn checked(run: &str, names: &BTreeSet<String>) -> String {
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    format!(
        r#"( {run} ); __dibs_rc=$?
__dibs_left=$(awk -v names=" {names} " '
    function out() {{ if (n != "" && s != "" && index(names, " " n " ")) print "  " n " from " s }}
    /^\[\[package\]\]/ {{ out(); n = ""; s = "" }}
    /^name = / {{ n = $3; gsub(/"/, "", n) }}
    /^source = / {{ s = $3; gsub(/"/, "", s) }}
    END {{ out() }}' Cargo.lock 2>/dev/null)
if [ -n "$__dibs_left" ]; then
    echo "dibs: the pin did not take. cargo still builds these from where they came before:" >&2
    echo "$__dibs_left" >&2
    echo "  Most often the pinned tree's version does not meet the requirement the dependency states." >&2
    exit 3
fi
exit $__dibs_rc"#,
        names = names.join(" ")
    )
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!("git {} in {}: {}", args.join(" "), dir.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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
                ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n".to_string()),
                ("crates/cubecl/Cargo.toml", "[package]\nname = \"cubecl\"\nversion.workspace = true\n".to_string()),
                ("crates/cubecl-core/Cargo.toml", "[package]\nname = \"cubecl-core\"\n".to_string()),
                ("crates/cubecl-cpp/Cargo.toml", "[package]\nname = \"cubecl-cpp\"\n".to_string()),
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
        let names: Vec<&str> = s["https://github.com/tracel-ai/cubecl"].iter().map(String::as_str).collect();
        assert_eq!(names, ["cubecl", "cubecl-core"], "cubecl-cpp is not in the graph, so a patch for it would only warn");
        let text = config(&[("/m/ws/cubecl/local-k".into(), cubecl(), s)]);
        assert_eq!(
            text,
            "[patch.\"https://github.com/tracel-ai/cubecl\"]\ncubecl = { path = \"/m/ws/cubecl/local-k/crates/cubecl\" }\ncubecl-core = { path = \"/m/ws/cubecl/local-k/crates/cubecl-core\" }\n"
        );
        assert!(toml::from_str::<toml::Value>(&text).is_ok());
    }

    #[test]
    fn a_crate_from_the_registry_is_patched_as_crates_io() {
        let serde = crates([("Cargo.toml", "[package]\nname = \"serde\"\n".to_string())].iter().map(|(p, t)| (*p, t.clone())));
        let s = sources(LOCK, &serde).unwrap();
        assert!(config(&[("/t".into(), serde, s)]).starts_with("[patch.crates-io]\nserde = { path = \"/t\" }"));
    }

    #[test]
    fn a_build_that_still_takes_a_pinned_crate_from_git_fails() {
        let dir = std::env::temp_dir().join(format!("dibs-pin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.lock"), LOCK).unwrap();
        let names: BTreeSet<String> = ["cubecl".to_string()].into();
        let run = |names: &BTreeSet<String>| {
            std::process::Command::new("bash").arg("-c").arg(checked("true", names)).current_dir(&dir).output().unwrap()
        };
        let out = run(&names);
        assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stderr).contains("cubecl from git+https://github.com/tracel-ai/cubecl"));
        assert_eq!(run(&["cubek".to_string()].into()).status.code(), Some(0), "a path crate is what a pin leaves");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
