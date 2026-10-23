//! Git dependencies a machine cannot fetch for itself.
//!
//! A machine holds no credentials, so a private repo pinned in `Cargo.lock` fails the build a
//! second in, after the job has queued behind everything else. This checkout's cargo usually
//! has that commit already, or can fetch it with the credentials here, so it is sent ahead of the
//! build, and only when the machine lacks it. Git objects are named by content, so adding files
//! never changes one already there.

use crate::git::Git;
use dibs_format::lockfile::Package;
use std::{path::PathBuf, process::Command};

/// A pinned commit and the directory in this machine's cargo git cache that holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Db {
    pub name: String,
    pub path: PathBuf,
    pub commit: String,
}

/// A git dependency a lockfile pins.
#[derive(Debug, Clone, PartialEq)]
pub struct GitPin {
    pub repo: String,
    pub url: String,
    pub commit: String,
}

impl GitPin {
    /// Every git source in a lockfile, once each.
    pub fn all(lock: &str) -> Vec<GitPin> {
        let mut out: Vec<GitPin> = Vec::new();
        for package in Package::all(lock) {
            let Some(src) = package
                .source
                .as_deref()
                .and_then(|s| s.strip_prefix("git+"))
            else {
                continue;
            };
            let Some((url, commit)) = src.rsplit_once('#') else {
                continue;
            };
            let url = url.split('?').next().unwrap_or(url);
            let name = url.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            let name = name.trim_end_matches(".git").to_string();
            if name.is_empty() || commit.len() < 40 {
                continue;
            }
            if !out.iter().any(|p| p.repo == name && p.commit == commit) {
                out.push(GitPin {
                    repo: name,
                    url: url.to_string(),
                    commit: commit.to_string(),
                });
            }
        }
        out
    }
}

/// A cargo home, whose git cache may hold the commits a machine cannot fetch.
pub struct CargoHome {
    path: PathBuf,
}

impl CargoHome {
    pub fn here() -> CargoHome {
        let path = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cargo")
            });
        CargoHome { path }
    }

    /// The pinned commits this checkout's cargo can supply. Cargo names a cache directory after the
    /// repo plus a hash of its URL, which matches the machine's only when both cargos hash the same
    /// way. One URL spelled two ways gets two directories, and the commit may be in either, so every
    /// directory holding it is offered rather than the first: the machine reads exactly one of them.
    pub fn dbs(&self, pins: &[GitPin]) -> Vec<Db> {
        let mut out = Vec::new();
        for GitPin { repo, commit, .. } in pins {
            for (name, path) in self.dirs(repo) {
                if holds(&path, commit) {
                    out.push(Db {
                        name,
                        path,
                        commit: commit.clone(),
                    });
                }
            }
        }
        out
    }

    /// Fetches, with this computer's credentials, each commit of a repo `private` says the
    /// machines cannot read and no directory here holds, into every directory cargo keeps for
    /// that repo. A repo cargo never fetched here has none, and its name hashes the URL as cargo
    /// does, so that one is left to the machine.
    pub fn fetch_private(&self, pins: &[GitPin], private: impl Fn(&str) -> bool) {
        for GitPin { repo, url, commit } in pins {
            let dirs = self.dirs(repo);
            if dirs.is_empty() || dirs.iter().any(|(_, p)| holds(p, commit)) || !private(url) {
                continue;
            }
            eprintln!("dibs: fetching {repo} at {commit:.8} here, since the machines cannot");
            for (_, path) in dirs {
                let fetched = Command::new("git")
                    .arg("-C")
                    .arg(&path)
                    .args([
                        "fetch",
                        "--quiet",
                        "--no-tags",
                        "--no-write-fetch-head",
                        url,
                    ])
                    .arg(format!("+{commit}:refs/commit/{commit}"))
                    .env("GIT_TERMINAL_PROMPT", "0")
                    .status()
                    .is_ok_and(|s| s.success());
                if !fetched {
                    eprintln!(
                        "dibs: fetching {url} failed here too, so the machine's cargo will fail on it"
                    );
                    break;
                }
            }
        }
    }

    /// The directories cargo keeps for a repo, one per spelling of its URL.
    fn dirs(&self, repo: &str) -> Vec<(String, PathBuf)> {
        let Ok(entries) = std::fs::read_dir(self.path.join("git/db")) else {
            return Vec::new();
        };
        let prefix = format!("{}-", repo.to_lowercase());
        entries
            .filter_map(|e| e.ok())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .filter(|(name, _)| {
                name.to_lowercase()
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| {
                        rest.len() == 16 && rest.chars().all(|c| c.is_ascii_hexdigit())
                    })
            })
            .collect()
    }
}

fn holds(db: &std::path::Path, commit: &str) -> bool {
    Git(db)
        .run(&["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, process::Command};

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
            GitPin::all(LOCK),
            vec![
                GitPin {
                    repo: "widget".into(),
                    url: "https://git.example.invalid/org/widget".into(),
                    commit: "2d1aeb21c0ccfe271f12d1209724c6012a133134".into()
                },
                GitPin {
                    repo: "gadget".into(),
                    url: "https://git.example.invalid/org/gadget.git".into(),
                    commit: "0123456789abcdef0123456789abcdef01234567".into()
                },
            ]
        );
    }

    fn sh(dir: &Path, cmd: &str) -> String {
        let o = Command::new("bash")
            .args(["-c", cmd])
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{cmd}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// A cargo home holding one repo's cache at one commit, as a bare repo the way cargo keeps it.
    fn home(name: &str) -> (PathBuf, String) {
        let root = std::env::temp_dir().join(format!("dibs-gitdeps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        sh(
            &work,
            "git init -q && git -c user.email=a@b -c user.name=t commit -q --allow-empty -m one",
        );
        let commit = sh(&work, "git rev-parse HEAD");
        std::fs::create_dir_all(root.join("cargo/git/db")).unwrap();
        sh(
            &root,
            "git clone -q --bare work cargo/git/db/widget-0123456789abcdef",
        );
        (root, commit)
    }

    #[test]
    fn only_commits_this_cargo_holds_are_offered() {
        let (root, commit) = home("local");
        let pins = vec![
            GitPin {
                repo: "widget".to_string(),
                url: String::new(),
                commit: commit.clone(),
            },
            GitPin {
                repo: "widget".to_string(),
                url: String::new(),
                commit: "f".repeat(40),
            },
        ];
        let cargo = CargoHome {
            path: root.join("cargo"),
        };
        let dbs = cargo.dbs(&pins);
        assert_eq!(dbs.len(), 1);
        assert_eq!(dbs[0].name, "widget-0123456789abcdef");
        assert_eq!(dbs[0].commit, commit);
        let _ = std::fs::remove_dir_all(&root);
    }

    // One URL spelled two ways is two cache directories, and the machine reads only one of them.
    #[test]
    fn every_directory_holding_the_commit_is_offered() {
        let (root, commit) = home("spellings");
        sh(
            &root,
            "git clone -q --bare work cargo/git/db/widget-fedcba9876543210",
        );
        let cargo = CargoHome {
            path: root.join("cargo"),
        };
        let pin = GitPin {
            repo: "widget".to_string(),
            url: String::new(),
            commit,
        };
        let dbs = cargo.dbs(&[pin]);
        let mut names: Vec<&str> = dbs.iter().map(|d| d.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["widget-0123456789abcdef", "widget-fedcba9876543210"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_private_commit_missing_here_is_fetched_into_the_repos_directory() {
        let (root, _) = home("fetch");
        sh(
            &root.join("work"),
            "git -c user.email=a@b -c user.name=t commit -q --allow-empty -m two",
        );
        let pin = GitPin {
            repo: "widget".to_string(),
            url: root.join("work").display().to_string(),
            commit: sh(&root.join("work"), "git rev-parse HEAD"),
        };
        let cargo = CargoHome {
            path: root.join("cargo"),
        };
        cargo.fetch_private(std::slice::from_ref(&pin), |_| false);
        assert!(cargo.dbs(std::slice::from_ref(&pin)).is_empty());
        cargo.fetch_private(std::slice::from_ref(&pin), |_| true);
        assert_eq!(cargo.dbs(&[pin]).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
