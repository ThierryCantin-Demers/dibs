//! Embeds the runner's source with its hash, and stamps the commit and the clone, so a call reads
//! nothing from the clone and the binary keeps working with it moved or gone.

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

    let tree = RunnerTree::of(&clone);
    for dir in RunnerTree::WATCHED {
        println!("cargo:rerun-if-changed={}", clone.join(dir).display());
    }
    let tar = tree.tar();
    let hash: String = Sha256::digest(&tar)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("cargo:rustc-env=DIBS_RUNNER_HASH={hash}");
    let mut gz = GzEncoder::new(Vec::new(), Compression::best());
    gz.write_all(&tar).expect("gzip into memory");
    fs::write(
        out.join("runner-source.tar.gz"),
        gz.finish().expect("gzip into memory"),
    )
    .expect("OUT_DIR is writable");

    println!("cargo:rustc-env=DIBS_CLONE={}", clone.display());
    if let Some(commit) = Head::of(&clone) {
        println!("cargo:rustc-env=DIBS_COMMIT={}", commit.short);
        for watched in commit.watched {
            println!("cargo:rerun-if-changed={}", watched.display());
        }
    }
}

/// The runner's source as a machine builds it: the two crates under `crates/`, with the
/// provisioning workspace's manifest, lock file and install script at the top.
struct RunnerTree {
    files: Vec<TreeFile>,
}

/// One file of the tree: where it goes in the tree, and its contents.
struct TreeFile {
    path: String,
    bytes: Vec<u8>,
}

impl RunnerTree {
    const WATCHED: [&str; 6] = [
        "Cargo.lock",
        "crates/dibs-runner/src",
        "crates/dibs-runner/provision",
        "crates/dibs-runner/Cargo.toml",
        "crates/dibs-format/src",
        "crates/dibs-format/Cargo.toml",
    ];

    fn of(clone: &Path) -> RunnerTree {
        let read =
            |path: &Path| fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let provision = clone.join("crates/dibs-runner/provision");
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
        let lock = String::from_utf8(read(&clone.join("Cargo.lock"))).expect("Cargo.lock is text");
        files.push(TreeFile {
            path: "Cargo.lock".to_string(),
            bytes: RunnerLock::cut(&lock).into_bytes(),
        });
        for crate_dir in ["crates/dibs-format", "crates/dibs-runner"] {
            let root = clone.join(crate_dir);
            files.push(TreeFile {
                path: format!("{crate_dir}/Cargo.toml"),
                bytes: read(&root.join("Cargo.toml")),
            });
            for source in RunnerTree::sources(&root.join("src")) {
                let relative = source.strip_prefix(&root).expect("under its crate");
                files.push(TreeFile {
                    path: format!("{crate_dir}/{}", relative.display()),
                    bytes: read(&source),
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        RunnerTree { files }
    }

    fn sources(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("a directory entry").path();
            match path.is_dir() {
                true => found.extend(RunnerTree::sources(&path)),
                false => found.push(path),
            }
        }
        found
    }

    /// A ustar archive with nothing in it but names and contents, so the same tree always packs to
    /// the same bytes and the same hash.
    fn tar(&self) -> Vec<u8> {
        let mut tar = Vec::new();
        for file in &self.files {
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

/// The clone's lock file cut to what the runner and the format reach, so a machine builds the
/// versions this client was built and tested with, and no second lock file can fall behind.
struct RunnerLock;

impl RunnerLock {
    const MEMBERS: [&str; 2] = ["dibs-format", "dibs-runner"];
    const PACKAGE: &str = "\n[[package]]\n";

    fn cut(lock: &str) -> String {
        let mut blocks = lock.split(RunnerLock::PACKAGE);
        let header = blocks.next().expect("a lock file starts with its header");
        let blocks: Vec<&str> = blocks.collect();
        let mut kept = vec![false; blocks.len()];
        let mut wanted: Vec<String> = RunnerLock::MEMBERS.iter().map(|m| m.to_string()).collect();
        while let Some(entry) = wanted.pop() {
            let mut words = entry.split(' ');
            let name = words.next().expect("a dependency names a package");
            let version = words.next();
            let found = blocks.iter().position(|block| {
                RunnerLock::field(block, "name") == Some(name)
                    && version.is_none_or(|v| RunnerLock::field(block, "version") == Some(v))
            });
            let at = found.unwrap_or_else(|| panic!("Cargo.lock has no package {entry}"));
            if !kept[at] {
                kept[at] = true;
                wanted.extend(RunnerLock::dependencies(blocks[at]));
            }
        }
        blocks
            .iter()
            .zip(kept)
            .filter(|(_, kept)| *kept)
            .fold(header.to_string(), |lock, (block, _)| {
                lock + RunnerLock::PACKAGE + block
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
