//! Embeds the runner's source and the client's, each with its hash, and stamps the commit and
//! the clone, so a call reads nothing from the clone and the binary keeps working with it moved
//! or gone.

use flate2::{Compression, write::GzEncoder};
use sha2::{Digest as _, Sha256};
use std::{
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let clone = manifest.join("../..");
    let clone = clone.canonicalize().unwrap_or(clone);
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));

    for (tree, name) in [
        (SourceTree::RUNNER, "RUNNER"),
        (SourceTree::CLIENT, "CLIENT"),
    ] {
        for watched in tree.watched() {
            println!("cargo:rerun-if-changed={}", clone.join(watched).display());
        }
        let hash = tree.pack(
            &clone,
            &out.join(format!("{}-source.tar.gz", name.to_lowercase())),
        );
        println!("cargo:rustc-env=DIBS_{name}_HASH={hash}");
    }

    println!("cargo:rustc-env=DIBS_CLONE={}", clone.display());
    if let Some(commit) = Head::of(&clone) {
        println!("cargo:rustc-env=DIBS_COMMIT={}", commit.short);
        for watched in commit.watched {
            println!("cargo:rerun-if-changed={}", watched.display());
        }
    }
}

/// A source a machine builds: crates under `crates/`, with a provisioning workspace's manifest,
/// lock file and install script at the top.
#[derive(Clone, Copy)]
struct SourceTree {
    /// Where the workspace manifest and install script are kept.
    provision: &'static str,
    /// The packages the lock file is cut to.
    roots: &'static [&'static str],
    crates: &'static [Member],
    /// Whether the cut lock file is carried again as `source.lock`. Cargo rewrites a lock cut
    /// from a larger workspace's, since features another member turns on there add packages, so
    /// a client built there packs its trees from this copy, to this binary's hashes.
    keeps_lock: bool,
}

/// A crate as a tree carries it: its files, and every file under its directories.
#[derive(Clone, Copy)]
struct Member {
    dir: &'static str,
    files: &'static [&'static str],
    walked: &'static [&'static str],
}

/// One file of the tree: where it goes in the tree, and its contents.
struct TreeFile {
    path: String,
    bytes: Vec<u8>,
}

impl SourceTree {
    const RUNNER: SourceTree = SourceTree {
        provision: "crates/dibs-runner/provision",
        roots: &["dibs-format", "dibs-runner"],
        keeps_lock: false,
        crates: &[
            Member {
                dir: "crates/dibs-format",
                files: &["Cargo.toml"],
                walked: &["src"],
            },
            Member {
                dir: "crates/dibs-runner",
                files: &["Cargo.toml"],
                walked: &["src"],
            },
        ],
    };

    /// The client, built on a machine to drive a detached batch there. It carries both
    /// provisioning directories, since its build script packs both trees again, to the same bytes.
    const CLIENT: SourceTree = SourceTree {
        provision: "crates/dibs/provision",
        roots: &["dibs"],
        keeps_lock: true,
        crates: &[
            Member {
                dir: "crates/dibs",
                files: &["Cargo.toml", "build.rs"],
                walked: &["src", "provision", "tests/live"],
            },
            Member {
                dir: "crates/dibs-format",
                files: &["Cargo.toml"],
                walked: &["src"],
            },
            Member {
                dir: "crates/dibs-runner",
                files: &["Cargo.toml"],
                walked: &["src", "provision"],
            },
        ],
    };

    const KEPT_LOCK: &str = "source.lock";

    fn watched(&self) -> Vec<String> {
        let mut watched = vec!["Cargo.lock".to_string(), self.provision.to_string()];
        for member in self.crates {
            for path in member.files.iter().chain(member.walked) {
                watched.push(format!("{}/{path}", member.dir));
            }
        }
        watched
    }

    /// Writes the tree gzipped to `to`, and gives its hash.
    fn pack(&self, clone: &Path, to: &Path) -> String {
        let tar = self.files(clone).tar();
        let hash: String = Sha256::digest(&tar)
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut gz = GzEncoder::new(Vec::new(), Compression::best());
        gz.write_all(&tar).expect("gzip into memory");
        fs::write(to, gz.finish().expect("gzip into memory")).expect("OUT_DIR is writable");
        hash
    }

    fn files(&self, clone: &Path) -> Files {
        let read =
            |path: &Path| fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let provision = clone.join(self.provision);
        let mut files: Vec<TreeFile> = [
            ("Cargo.toml", "workspace.toml"),
            ("install.sh", "install.sh"),
        ]
        .into_iter()
        .map(|(path, from)| TreeFile {
            path: path.to_string(),
            bytes: read(&provision.join(from)),
        })
        .collect();
        let kept = clone.join(SourceTree::KEPT_LOCK);
        let lock = match kept.is_file() {
            true => read(&kept),
            false => read(&clone.join("Cargo.lock")),
        };
        let lock = String::from_utf8(lock).expect("a lock file is text");
        let cut = TreeLock::cut(&lock, self.roots).into_bytes();
        if self.keeps_lock {
            files.push(TreeFile {
                path: SourceTree::KEPT_LOCK.to_string(),
                bytes: cut.clone(),
            });
        }
        files.push(TreeFile {
            path: "Cargo.lock".to_string(),
            bytes: cut,
        });
        for member in self.crates {
            let root = clone.join(member.dir);
            for file in member.files {
                files.push(TreeFile {
                    path: format!("{}/{file}", member.dir),
                    bytes: read(&root.join(file)),
                });
            }
            for walked in member.walked {
                for source in SourceTree::sources(&root.join(walked)) {
                    let relative = source.strip_prefix(&root).expect("under its crate");
                    files.push(TreeFile {
                        path: format!("{}/{}", member.dir, relative.display()),
                        bytes: read(&source),
                    });
                }
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Files(files)
    }

    fn sources(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("a directory entry").path();
            match path.is_dir() {
                true => found.extend(SourceTree::sources(&path)),
                false => found.push(path),
            }
        }
        found
    }
}

/// A tree's files, in the order they are packed.
struct Files(Vec<TreeFile>);

impl Files {
    /// A ustar archive with nothing in it but names and contents, so the same tree always packs to
    /// the same bytes and the same hash.
    fn tar(&self) -> Vec<u8> {
        let mut tar = Vec::new();
        for file in &self.0 {
            let mut header = [0u8; 512];
            let name = file.path.as_bytes();
            assert!(
                name.len() < 100,
                "{} is too long a name for ustar",
                file.path
            );
            header[..name.len()].copy_from_slice(name);
            let mut field = |at: usize, text: &str| {
                header[at..at + text.len()].copy_from_slice(text.as_bytes());
            };
            field(100, "0000644\0");
            field(108, "0000000\0");
            field(116, "0000000\0");
            field(124, &format!("{:011o}\0", file.bytes.len()));
            field(136, "00000000000\0");
            field(148, "        ");
            field(156, "0");
            field(257, "ustar\0");
            field(263, "00");
            let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
            header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            tar.extend_from_slice(&header);
            tar.extend_from_slice(&file.bytes);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.resize(tar.len() + 1024, 0);
        tar
    }
}

/// The clone's lock file cut to what a tree's packages reach, so a machine builds the versions
/// this client was built and tested with, and no second lock file can fall behind.
struct TreeLock;

impl TreeLock {
    const PACKAGE: &str = "\n[[package]]\n";

    fn cut(lock: &str, roots: &[&str]) -> String {
        let mut blocks = lock.split(TreeLock::PACKAGE);
        let header = blocks.next().expect("a lock file starts with its header");
        let blocks: Vec<&str> = blocks.collect();
        let mut kept = vec![false; blocks.len()];
        let mut wanted: Vec<String> = roots.iter().map(|m| m.to_string()).collect();
        while let Some(entry) = wanted.pop() {
            let mut words = entry.split(' ');
            let name = words.next().expect("a dependency names a package");
            let version = words.next();
            let found = blocks.iter().position(|block| {
                TreeLock::field(block, "name") == Some(name)
                    && version.is_none_or(|v| TreeLock::field(block, "version") == Some(v))
            });
            let at = found.unwrap_or_else(|| panic!("Cargo.lock has no package {entry}"));
            if !kept[at] {
                kept[at] = true;
                wanted.extend(TreeLock::dependencies(blocks[at]));
            }
        }
        blocks
            .iter()
            .zip(kept)
            .filter(|(_, kept)| *kept)
            .fold(header.to_string(), |lock, (block, _)| {
                lock + TreeLock::PACKAGE + block
            })
    }

    fn field<'a>(block: &'a str, key: &str) -> Option<&'a str> {
        block
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(" = \""))
            .map(|rest| rest.trim_end_matches('"'))
    }

    fn dependencies(block: &str) -> Vec<String> {
        block
            .lines()
            .skip_while(|line| *line != "dependencies = [")
            .skip(1)
            .take_while(|line| *line != "]")
            .map(|line| {
                line.trim()
                    .trim_end_matches(',')
                    .trim_matches('"')
                    .to_string()
            })
            .collect()
    }
}

/// The commit a clone is at, and the files whose change moves it.
struct Head {
    short: String,
    watched: Vec<PathBuf>,
}

impl Head {
    fn of(clone: &Path) -> Option<Head> {
        let short = Head::git(clone, &["rev-parse", "--short", "HEAD"])?;
        let own = PathBuf::from(Head::git(clone, &["rev-parse", "--absolute-git-dir"])?);
        let common = Head::git(clone, &["rev-parse", "--git-common-dir"])
            .map(|dir| clone.join(dir))
            .unwrap_or_else(|| own.clone());
        let watched = [
            own.join("HEAD"),
            common.join("refs/heads"),
            common.join("packed-refs"),
        ]
        .into_iter()
        .filter(|p| p.exists())
        .collect();
        Some(Head { short, watched })
    }

    fn git(clone: &Path, args: &[&str]) -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(clone)
            .args(args)
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?.trim_end().to_string();
        (out.status.success() && !text.is_empty()).then_some(text)
    }
}
