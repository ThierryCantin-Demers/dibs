//! Embeds the machine half and stamps the commit and the clone, so a call reads nothing from the
//! clone and the binary keeps working with the clone moved or gone.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let clone = manifest.join("../..");
    let clone = clone.canonicalize().unwrap_or(clone);
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));

    let half = clone.join("lib/machine");
    println!("cargo:rerun-if-changed={}", half.display());
    fs::write(out.join("machine-half.sh"), MachineHalf::read(&half)).expect("OUT_DIR is writable");

    println!("cargo:rustc-env=DIBS_CLONE={}", clone.display());
    if let Some(commit) = Head::of(&clone) {
        println!("cargo:rustc-env=DIBS_COMMIT={}", commit.short);
        for watched in commit.watched {
            println!("cargo:rerun-if-changed={}", watched.display());
        }
    }
}

/// `lib/machine`'s scripts, in the order the machine reads them.
struct MachineHalf;

impl MachineHalf {
    fn read(dir: &Path) -> String {
        let mut parts: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "sh"))
            .collect();
        parts.sort();
        parts
            .iter()
            .map(|part| {
                fs::read_to_string(part).unwrap_or_else(|e| panic!("{}: {e}", part.display()))
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
