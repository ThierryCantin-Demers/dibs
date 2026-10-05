use crate::{
    settings::home,
    tree::clocks::{Clocks, Removal},
};
use dibs_format::wire::SOURCE_HASH;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

const DAY: u64 = 86400;
/// What every build of a runner holds while it builds, and a sweep while it collects.
const BUILD_LOCK: &str = ".build.lock";
/// The target every version's build shares.
const TARGET: &str = ".target";
/// What a build unpacks and leaves when it dies before cleaning up.
const LEFTOVERS: [&str; 2] = [".src.", ".tree."];
/// A version's binary, being copied in, before it is renamed into place.
const COPYING: &str = ".dibs-runner.";

/// The runners built on this machine: a directory per source hash, the target their builds
/// share, and what a build unpacks beside them.
pub struct Runners {
    pub dir: PathBuf,
    /// This process's own version, which is never collected.
    pub own: Option<String>,
}

/// What a sweep may collect of the runners, judged while it holds the build lock, so nothing a
/// build is using is among it.
pub struct Judged {
    _lock: File,
    pub entries: Vec<Entry>,
}

/// One version, leftover or target, and whether it is past its keep.
pub struct Entry {
    pub path: PathBuf,
    pub past: bool,
}

impl Runners {
    pub fn here() -> Runners {
        Runners {
            dir: home().join(".cache/dibs/runner"),
            own: SOURCE_HASH.map(str::to_string),
        }
    }

    /// Versions a later one replaced and that nobody has installed for the keep, since a client
    /// that has not updated builds its own again; what a dead build left; and the shared target
    /// once no build has used it for the cache's keep. None while a build runs.
    pub fn judged(&self, clocks: &Clocks, now: u64) -> Option<Judged> {
        let lock = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(BUILD_LOCK))
            .ok()?;
        lock.try_lock().ok()?;
        let versions: Vec<(PathBuf, u64)> = fs::read_dir(&self.dir)
            .map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.is_dir() && version(p))
            .map(|p| {
                let installed =
                    mtime(&p.join("dibs-runner")).unwrap_or_else(|| mtime(&p).unwrap_or(now));
                (p, installed)
            })
            .collect();
        let newest = versions.iter().map(|(_, at)| *at).max();
        let mut entries = Vec::new();
        for (path, installed) in &versions {
            let own = self
                .own
                .as_deref()
                .is_some_and(|own| path.file_name().is_some_and(|n| n == own));
            entries.push(Entry {
                past: !own
                    && Some(*installed) != newest
                    && now.saturating_sub(*installed) / DAY > clocks.keep_days,
                path: path.clone(),
            });
            entries.extend(
                named(path, &[COPYING])
                    .into_iter()
                    .map(|path| Entry { path, past: true }),
            );
        }
        entries.extend(
            named(&self.dir, &LEFTOVERS)
                .into_iter()
                .map(|path| Entry { path, past: true }),
        );
        let target = self.dir.join(TARGET);
        if target.is_dir() {
            let built = mtime(&target.join("release"))
                .or_else(|| mtime(&target))
                .unwrap_or(now);
            entries.push(Entry {
                past: now.saturating_sub(built) / DAY > clocks.target_keep_days,
                path: target,
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Some(Judged {
            _lock: lock,
            entries,
        })
    }

    /// What is past its keep removed, silently unless it cannot all go.
    pub fn collect(&self, clocks: &Clocks, now: u64, removal: &Removal) {
        if let Some(judged) = self.judged(clocks, now) {
            for entry in judged.entries.iter().filter(|e| e.past) {
                removal.path(&entry.path);
            }
        }
    }
}

/// A directory named by a runner source's hash.
fn version(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.len() == 16 && n.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The entries of `dir` whose names start with one of `prefixes`.
fn named(dir: &Path, prefixes: &[&str]) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    prefixes.iter().any(|p| name.starts_with(p))
                })
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default()
}

fn mtime(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.mtime().max(0) as u64)
}
