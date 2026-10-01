//! Git dependencies a machine cannot fetch for itself.
//!
//! A machine holds no credentials, so a private repo pinned in `Cargo.lock` fails the build a
//! second in, after the job has queued behind everything else. This checkout's cargo usually
//! has that commit already, so it is sent ahead of the build, and only when the machine lacks
//! it. Git objects are named by content, so adding files never changes one already there.

use std::path::{Path, PathBuf};

/// A pinned commit and the directory in this machine's cargo git cache that holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Db {
    pub name: String,
    pub path: PathBuf,
    pub commit: String,
}

/// `(repo name, commit)` for every git source in a lockfile, once each.
pub fn pinned(lock: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in lock.lines() {
        let Some(src) = line.trim().strip_prefix("source = \"git+") else { continue };
        let src = src.trim_end_matches('"');
        let Some((url, commit)) = src.rsplit_once('#') else { continue };
        let url = url.split('?').next().unwrap_or(url);
        let name = url.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        let name = name.trim_end_matches(".git").to_string();
        if name.is_empty() || commit.len() < 40 {
            continue;
        }
        let pair = (name, commit.to_string());
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    out
}

pub fn cargo_home() -> PathBuf {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cargo"))
}

/// The pinned commits this checkout's cargo can supply. Cargo names a cache directory after the
/// repo plus a hash of its URL, which matches the machine's only when both cargos hash the same
/// way. One URL spelled two ways gets two directories, and the commit may be in either, so every
/// directory holding it is offered rather than the first: the machine reads exactly one of them.
pub fn local(cargo_home: &Path, pins: &[(String, String)]) -> Vec<Db> {
    let Ok(entries) = std::fs::read_dir(cargo_home.join("git/db")) else { return Vec::new() };
    let dirs: Vec<(String, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    let mut out = Vec::new();
    for (repo, commit) in pins {
        let prefix = format!("{}-", repo.to_lowercase());
        for (name, path) in &dirs {
            let hash = name.to_lowercase();
            let Some(rest) = hash.strip_prefix(&prefix) else { continue };
            if rest.len() != 16 || !rest.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let has = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if has {
                out.push(Db { name: name.clone(), path: path.clone(), commit: commit.clone() });
            }
        }
    }
    out
}

/// Shell for the setup job: names each commit the machine lacks, and where its cargo keeps
/// them. Every command is guarded, because the setup runs under `set -e`.
pub fn check_script(dbs: &[Db]) -> String {
    if dbs.is_empty() {
        return String::new();
    }
    let mut s = String::from("GITDB=${CARGO_HOME:-$HOME/.cargo}/git/db\necho \"DIBS-GITDB $GITDB\"\n");
    for db in dbs {
        s.push_str(&format!(
            "git -C \"$GITDB/{n}\" cat-file -e '{c}^{{commit}}' 2>/dev/null || echo 'DIBS-GITMISSING {n} {c}'\n",
            n = db.name,
            c = db.commit
        ));
    }
    s
}

/// The machine's cache directory and which of `dbs` it reported missing.
pub fn missing<'a>(setup_output: &str, dbs: &'a [Db]) -> (Option<String>, Vec<&'a Db>) {
    let mut gitdb = None;
    let mut out = Vec::new();
    for line in setup_output.lines() {
        if let Some(v) = line.strip_prefix("DIBS-GITDB ") {
            gitdb = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("DIBS-GITMISSING ") {
            let mut it = v.split_whitespace();
            if let (Some(n), Some(c)) = (it.next(), it.next()) {
                if let Some(db) = dbs.iter().find(|d| d.name == n && d.commit == c) {
                    out.push(db);
                }
            }
        }
    }
    (gitdb, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const LOCK: &str = r#"
[[package]]
name = "widget"
version = "0.1.0"
source = "git+https://git.example.invalid/org/widget?rev=2d1aeb21c0ccfe271f12d1209724c6012a133134#2d1aeb21c0ccfe271f12d1209724c6012a133134"

[[package]]
name = "widget-sys"
version = "0.1.0"
source = "git+https://git.example.invalid/org/widget?rev=2d1aeb21c0ccfe271f12d1209724c6012a133134#2d1aeb21c0ccfe271f12d1209724c6012a133134"

[[package]]
name = "gadget"
source = "git+https://git.example.invalid/org/gadget.git?branch=main#0123456789abcdef0123456789abcdef01234567"

[[package]]
name = "serde"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

    #[test]
    fn a_lockfile_names_each_git_commit_once_and_skips_the_registry() {
        assert_eq!(
            pinned(LOCK),
            vec![
                ("widget".into(), "2d1aeb21c0ccfe271f12d1209724c6012a133134".into()),
                ("gadget".into(), "0123456789abcdef0123456789abcdef01234567".into()),
            ]
        );
    }

    fn sh(dir: &Path, cmd: &str) -> String {
        let o = Command::new("bash").args(["-c", cmd]).current_dir(dir).output().unwrap();
        assert!(o.status.success(), "{cmd}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// A cargo home holding one repo's cache at one commit, as a bare repo the way cargo keeps it.
    fn home(name: &str) -> (PathBuf, String) {
        let root = std::env::temp_dir().join(format!("dibs-gitdeps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        sh(&work, "git init -q && git -c user.email=a@b -c user.name=t commit -q --allow-empty -m one");
        let commit = sh(&work, "git rev-parse HEAD");
        std::fs::create_dir_all(root.join("cargo/git/db")).unwrap();
        sh(&root, "git clone -q --bare work cargo/git/db/widget-0123456789abcdef");
        (root, commit)
    }

    #[test]
    fn only_commits_this_cargo_holds_are_offered() {
        let (root, commit) = home("local");
        let pins = vec![("widget".to_string(), commit.clone()), ("widget".to_string(), "f".repeat(40))];
        let dbs = local(&root.join("cargo"), &pins);
        assert_eq!(dbs.len(), 1);
        assert_eq!(dbs[0].name, "widget-0123456789abcdef");
        assert_eq!(dbs[0].commit, commit);
        let _ = std::fs::remove_dir_all(&root);
    }

    // One URL spelled two ways is two cache directories, and the machine reads only one of them.
    #[test]
    fn every_directory_holding_the_commit_is_offered() {
        let (root, commit) = home("spellings");
        sh(&root, "git clone -q --bare work cargo/git/db/widget-fedcba9876543210");
        let dbs = local(&root.join("cargo"), &[("widget".to_string(), commit)]);
        let mut names: Vec<&str> = dbs.iter().map(|d| d.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["widget-0123456789abcdef", "widget-fedcba9876543210"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_machine_reports_only_what_it_lacks() {
        let (root, commit) = home("remote");
        let dbs = local(&root.join("cargo"), &[("widget".to_string(), commit.clone())]);
        let absent = Db { name: "other-0123456789abcdef".into(), path: root.clone(), commit: "e".repeat(40) };
        let all = vec![dbs[0].clone(), absent.clone()];
        let script = format!("set -eu\n{}", check_script(&all));
        let out = Command::new("bash")
            .args(["-c", &script])
            .env("CARGO_HOME", root.join("cargo"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let (gitdb, gone) = missing(&text, &all);
        assert_eq!(gitdb.as_deref(), Some(root.join("cargo/git/db").to_str().unwrap()));
        assert_eq!(gone, vec![&absent]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
