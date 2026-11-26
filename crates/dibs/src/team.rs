//! The team's machines, as the dibspatcher keeps them: a copy here, refreshed by `dibs --update`
//! and once it is a day old, so a call still places from it while the dibspatcher is down.

use crate::{
    inventory::{Inventory, InventoryError},
    paths::Paths,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// How old the copy may grow before a call fetches it again.
const FRESH_FOR: Duration = Duration::from_secs(24 * 3600);
/// How long a failed fetch is not tried again, since each try can wait out ssh's timeout.
const RETRY_AFTER: Duration = Duration::from_secs(3600);
const FETCHED: &str = ".fetched";
const TRIED: &str = ".tried";
/// The dibspatcher the copy came from: a copy from another, or with none set up, is not read.
const FROM: &str = ".from";
/// A copy a detached batch carried to its machine, read as it is and never fetched there.
pub const CARRIED: &str = ".carried";

pub struct Team {
    dir: Option<PathBuf>,
}

impl Team {
    pub fn at(paths: &Paths) -> Team {
        Team { dir: paths.team() }
    }

    /// Where the copy is kept, when it is read from the dibspatcher set up here.
    pub fn kept(&self) -> Option<&Path> {
        self.dir.as_deref().filter(|_| self.load().is_some())
    }

    /// The copy, when there is one that reads and came from the dibspatcher set up here; one that
    /// does not read is left out, never fatal, since it is only ever written once it read.
    pub fn load(&self) -> Option<Inventory> {
        let dir = self.dir.as_ref()?;
        recover(dir);
        if !dir.join(CARRIED).exists() {
            let from = std::fs::read_to_string(dir.join(FROM)).ok()?;
            if Some(from) != crate::dispatch::dibspatcher_id() {
                return None;
            }
        }
        Inventory::load(&dir.join("machines.toml")).ok().flatten()
    }

    /// Fetches the team's inventory and keeps it in place of the copy. None when no dibspatcher
    /// is set up here; otherwise how many machines it has, or why it was not kept.
    pub fn refresh(&self) -> Option<Result<usize, String>> {
        let dir = self.dir.as_ref()?;
        recover(dir);
        let files = crate::dispatch::team_files()?;
        let from = crate::dispatch::dibspatcher_id().unwrap_or_default();
        let kept = files.and_then(|files| keep(dir, &files, &from));
        if kept.is_err() && std::fs::create_dir_all(dir).is_ok() {
            let _ = std::fs::write(dir.join(TRIED), &from);
        }
        Some(kept)
    }

    /// Refreshes a copy a day old, saying on stderr when that fails; the old copy stays.
    pub fn refresh_if_stale(&self) {
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        recover(dir);
        if dir.join(CARRIED).exists() {
            return;
        }
        let id = crate::dispatch::dibspatcher_id();
        let from = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        let fresh = from(FROM).as_deref() == id.as_deref() && younger(dir, FETCHED, FRESH_FOR);
        let tried = from(TRIED).as_deref() == id.as_deref() && younger(dir, TRIED, RETRY_AFTER);
        if fresh || tried {
            return;
        }
        if let Some(Err(why)) = self.refresh() {
            eprintln!("dibs: the team's machines were not refreshed: {why}");
        }
    }
}

/// Writes the files beside the copy, and puts them in its place only once they read as an
/// inventory.
fn keep(dir: &Path, files: &BTreeMap<String, String>, from: &str) -> Result<usize, String> {
    let parent = dir.parent().ok_or("the team's copy has no directory")?;
    let pid = std::process::id();
    let fresh = parent.join(format!(".team.new-{pid}"));
    let _ = std::fs::remove_dir_all(&fresh);
    let written = (|| -> Result<usize, String> {
        std::fs::create_dir_all(fresh.join("machines.d")).map_err(|e| e.to_string())?;
        for (name, text) in files {
            if !kept_name(name) {
                return Err(format!("the dibspatcher sent a file named {name:?}"));
            }
            std::fs::write(fresh.join(name), text).map_err(|e| e.to_string())?;
        }
        let read = Inventory::load(&fresh.join("machines.toml"))
            .map_err(|e: InventoryError| e.to_string())?;
        Ok(read.map_or(0, |i| i.machines.len()))
    })();
    let count = match written {
        Ok(count) => count,
        Err(why) => {
            let _ = std::fs::remove_dir_all(&fresh);
            return Err(why);
        }
    };
    stamp(&fresh, FETCHED);
    std::fs::write(fresh.join(FROM), from).map_err(|e| e.to_string())?;
    let old = parent.join(format!(".team.old-{pid}"));
    let _ = std::fs::rename(dir, &old);
    let placed = std::fs::rename(&fresh, dir).map_err(|e| format!("{}: {e}", dir.display()));
    if placed.is_err() {
        let _ = std::fs::rename(&old, dir);
        let _ = std::fs::remove_dir_all(&fresh);
    }
    let _ = std::fs::remove_dir_all(&old);
    placed.map(|()| count)
}

/// Puts back a copy that `keep` moved aside and did not live to replace.
fn recover(dir: &Path) {
    if dir.exists() {
        return;
    }
    let Some(parent) = dir.parent() else {
        return;
    };
    let aside = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".team.old-"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    if let Some(aside) = aside {
        let _ = std::fs::rename(aside.path(), dir);
    }
}

/// `machines.toml`, or one `machines.d/<name>.toml`: nothing that climbs out of the copy.
fn kept_name(name: &str) -> bool {
    let plain = |file: &str| {
        file.ends_with(".toml")
            && !file.starts_with('.')
            && !file.contains(['/', '\\'])
            && !file.is_empty()
    };
    name == "machines.toml" || name.strip_prefix("machines.d/").is_some_and(plain)
}

fn stamp(dir: &Path, name: &str) {
    if std::fs::create_dir_all(dir).is_ok() {
        let _ = std::fs::write(dir.join(name), "");
    }
}

fn younger(dir: &Path, name: &str, than: Duration) -> bool {
    std::fs::metadata(dir.join(name))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_some_and(|age| age < than)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs_read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn only_inventory_files_are_kept() {
        assert!(kept_name("machines.toml"));
        assert!(kept_name("machines.d/leased.toml"));
        for name in [
            "machines.d/../x.toml",
            "machines.d/.hidden.toml",
            "machines.d/a/b.toml",
            "machines.d/notes.txt",
            "../machines.toml",
            "fleet.toml",
        ] {
            assert!(!kept_name(name), "{name}");
        }
    }

    #[test]
    fn a_copy_that_does_not_read_leaves_the_old_one() {
        let root = std::env::temp_dir().join(format!("dibs-team-{}", std::process::id()));
        let dir = root.join("team");
        let _ = std::fs::remove_dir_all(&root);
        let good = BTreeMap::from([(
            "machines.toml".to_string(),
            "[machine.a]\nssh = \"a\"\nroles = [\"compile\"]\n".to_string(),
        )]);
        assert_eq!(keep(&dir, &good, "dibs@a"), Ok(1));
        assert!(younger(&dir, FETCHED, FRESH_FOR));
        let bad = BTreeMap::from([("machines.toml".to_string(), "[machine.a\n".to_string())]);
        assert!(keep(&dir, &bad, "dibs@a").is_err());
        assert_eq!(
            Inventory::load(&dir.join("machines.toml"))
                .unwrap()
                .map(|i| i.machines.len()),
            Some(1),
            "the copy that read is still there"
        );
        assert_eq!(fs_read(&dir.join(FROM)), "dibs@a");
        std::fs::rename(&dir, root.join(".team.old-1")).unwrap();
        assert_eq!(
            Team {
                dir: Some(dir.clone())
            }
            .load()
            .map(|i| i.machines.len()),
            None,
            "put back, then refused for want of a dibspatcher here"
        );
        assert_eq!(
            fs_read(&dir.join(FROM)),
            "dibs@a",
            "the copy moved aside is put back"
        );
        let leaving = BTreeMap::from([("machines.d/../../x.toml".to_string(), String::new())]);
        assert!(keep(&dir, &leaving, "dibs@a").is_err());
        assert!(!root.join("x.toml").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
