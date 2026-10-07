use crate::{
    settings::home,
    tree::{
        clocks::{Clocks, Contents as _, Fate, USED},
        copy::touch,
        sweep::{Kind, Section, Verdict},
    },
};
use dibs_format::wire::SOURCE_HASH;
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

/// What every build of a runner holds while it builds, and a sweep while it collects.
const BUILD_LOCK: &str = ".build.lock";
/// The target every version's build shares.
const TARGET: &str = ".target";
/// What a build unpacks, and the tree it was sent, both left when it dies before cleaning up.
const UNPACKED: &str = ".src.";
const SENT: &str = ".tree.";
/// A version's binary, being copied in, before it is renamed into place.
const COPYING: &str = ".dibs-runner.";
/// Hex digits in a source hash, which names a version.
const HASH_DIGITS: usize = 16;

/// The runners built on this machine: a directory per source hash, the target their builds
/// share, and what a build unpacks beside them.
pub struct Runners {
    dir: PathBuf,
    /// This process's own version, which is never collected.
    own: Option<String>,
}

impl Runners {
    pub fn here() -> Runners {
        Runners::in_home(&home())
    }

    pub fn in_home(home: &Path) -> Runners {
        Runners {
            dir: home.join(".cache/dibs/runner"),
            own: SOURCE_HASH.map(str::to_string),
        }
    }

    /// Whether `name` is a source hash, as a version's directory is named.
    pub fn names_a_version(name: &str) -> bool {
        name.len() == HASH_DIGITS && name.bytes().all(|b| b.is_ascii_hexdigit())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn binary(&self, hash: &str) -> PathBuf {
        self.dir.join(hash).join("dibs-runner")
    }

    pub fn target(&self) -> PathBuf {
        self.dir.join(TARGET)
    }

    pub fn build_lock(&self) -> PathBuf {
        self.dir.join(BUILD_LOCK)
    }

    /// Where a build by `pid` unpacks the tree it was sent.
    pub fn unpacked(&self, hash: &str, pid: u32) -> PathBuf {
        self.dir.join(format!("{UNPACKED}{hash}.{pid}"))
    }

    /// Where a build by `pid` keeps the tree it was sent.
    pub fn sent(&self, hash: &str, pid: u32) -> PathBuf {
        self.dir.join(format!("{SENT}{hash}.{pid}.tar.gz"))
    }

    /// This process's own version marked used, where it was installed: a version is kept by its
    /// use, since a client that has not updated runs it however long ago it was built.
    pub fn mark_used(&self) {
        if let Some(own) = &self.own {
            self.mark(own);
        }
    }

    /// `version` marked used where it was installed.
    pub fn mark(&self, version: &str) {
        if self.binary(version).is_file() {
            let _ = touch(&self.dir.join(version).join(USED));
        }
    }

    /// Versions a later one replaced and that nobody has used for the keep, since a client that
    /// has not updated builds its own again; what a dead build left; and the shared target once
    /// no build has used it for the cache's keep. The newest installed stays, since it is what
    /// builds the next. Judged holding the build lock, which the section keeps until what is past
    /// is gone, so nothing a build is using is among it; None while a build runs.
    pub fn judged(&self, clocks: &Clocks, now: u64) -> Option<Section> {
        if !self.dir.is_dir() {
            return Some(Section::holding(Kind::Runners, Vec::new(), None));
        }
        let lock = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.build_lock())
            .ok()?;
        lock.try_lock().ok()?;
        let versions: Vec<Version> = self
            .dir
            .entries()
            .into_iter()
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(Runners::names_a_version)
            })
            .map(|path| Version::at(path, now))
            .collect();
        let newest = versions.iter().map(|v| v.installed).max();
        let mut entries = Vec::new();
        for version in &versions {
            let own = self
                .own
                .as_deref()
                .is_some_and(|own| version.path.file_name().is_some_and(|n| n == own));
            let past = !own
                && Some(version.installed) != newest
                && Clocks::days(now, version.used) > clocks.keep_days;
            entries.push(Verdict::of(
                version.path.clone(),
                Runners::fate(past),
                version.used,
            ));
            entries.extend(
                Runners::named(&version.path, &[COPYING])
                    .into_iter()
                    .map(|path| Verdict::of(path, Fate::Past, now)),
            );
        }
        entries.extend(
            Runners::named(&self.dir, &[UNPACKED, SENT])
                .into_iter()
                .map(|path| Verdict::of(path, Fate::Past, now)),
        );
        let target = self.target();
        if target.is_dir() {
            let built = Runners::mtime(&target.join("release"))
                .or_else(|| Runners::mtime(&target))
                .unwrap_or(now);
            let past = Clocks::days(now, built) > clocks.target_keep_days;
            entries.push(Verdict::of(target, Runners::fate(past), built));
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Some(Section::holding(Kind::Runners, entries, Some(lock)))
    }

    fn fate(past: bool) -> Fate {
        match past {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

    /// The entries of `dir` whose names start with one of `prefixes`.
    fn named(dir: &Path, prefixes: &[&str]) -> Vec<PathBuf> {
        dir.entries()
            .into_iter()
            .filter(|p| {
                p.file_name().is_some_and(|n| {
                    let name = n.to_string_lossy();
                    prefixes.iter().any(|prefix| name.starts_with(prefix))
                })
            })
            .collect()
    }

    fn mtime(path: &Path) -> Option<u64> {
        fs::metadata(path).ok().map(|m| m.mtime().max(0) as u64)
    }
}

/// An installed version, by when it was installed and last used.
struct Version {
    path: PathBuf,
    installed: u64,
    used: u64,
}

impl Version {
    fn at(path: PathBuf, now: u64) -> Version {
        let installed = Runners::mtime(&path.join("dibs-runner"))
            .unwrap_or_else(|| Runners::mtime(&path).unwrap_or(now));
        let used = Runners::mtime(&path.join(USED)).map_or(installed, |used| used.max(installed));
        Version {
            path,
            installed,
            used,
        }
    }
}
