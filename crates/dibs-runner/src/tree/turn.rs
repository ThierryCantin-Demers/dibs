use crate::{clock::Deadline, tree::builds::RETRY};
use std::{
    fs::{self, File},
    io,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

/// How many times a turn is taken again after the file it locked was removed meanwhile.
const TAKES: usize = 8;

/// A lock file dibs makes and holds exclusively: a prepare's, so that two never lay out one tree
/// or target at once, or a sweep's, while it removes one. A sweep that removed what a turn guards
/// removes the turn too, so every taker holds a turn only while its path still names that file.
pub struct Turn {
    #[allow(dead_code, reason = "held for its lock")]
    file: File,
    path: PathBuf,
}

/// A turn's file opened and not yet locked: a sweep may remove it, and another taker make the
/// next, before it is.
struct Opened {
    file: File,
    path: PathBuf,
}

impl Turn {
    /// The turn beside `path`: `.<name>.lock` in the same directory.
    pub fn beside(path: &Path) -> PathBuf {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        path.with_file_name(format!(".{name}.lock"))
    }

    /// A sent tree's, beside the repo's trees rather than in its nest, which a sweep removes
    /// whole.
    pub fn of_sent(trees: &Path, tree: &str, nest: Option<&str>) -> PathBuf {
        let nest = nest.map(|n| format!("-{n}")).unwrap_or_default();
        trees.join(format!(".{tree}{nest}.lock"))
    }

    /// `path`'s turn, made if missing, at once.
    pub fn now(path: &Path) -> Option<Turn> {
        for _ in 0..TAKES {
            let opened = Opened::create(path).ok()?;
            opened.file.try_lock().ok()?;
            if let Some(turn) = opened.named() {
                return Some(turn);
            }
        }
        None
    }

    /// `path`'s turn, made if missing, before `deadline`; None once it has passed.
    pub fn by(path: &Path, deadline: Deadline) -> io::Result<Option<Turn>> {
        for _ in 0..TAKES {
            let opened = Opened::create(path)?;
            let locked = match deadline.left() {
                None => opened.file.lock().map(|()| true)?,
                Some(_) => deadline.until(RETRY, || opened.file.try_lock().is_ok()),
            };
            if !locked {
                return Ok(None);
            }
            if let Some(turn) = opened.named() {
                return Ok(Some(turn));
            }
        }
        Err(io::Error::other(format!(
            "{} was removed under each of {TAKES} takes",
            path.display()
        )))
    }

    /// Removes the file while still holding it, so a taker that opened it meanwhile takes the
    /// next one instead.
    pub fn remove(self) {
        let _ = fs::remove_file(&self.path);
    }

    /// Removed once `guarded` is gone, and otherwise let go.
    pub fn remove_with(self, guarded: &Path) {
        if fs::symlink_metadata(guarded).is_err() {
            self.remove();
        }
    }
}

impl Opened {
    fn create(path: &Path) -> io::Result<Opened> {
        Ok(Opened {
            file: File::create(path)?,
            path: path.to_path_buf(),
        })
    }

    /// Locked, as the turn at its path; None when the path names another file by then.
    fn named(self) -> Option<Turn> {
        let (held, named) = (self.file.metadata().ok()?, fs::metadata(&self.path).ok()?);
        (held.dev() == named.dev() && held.ino() == named.ino()).then_some(Turn {
            file: self.file,
            path: self.path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dibs-turn-{name}.{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_taker_that_opened_a_turn_a_sweep_then_removed_takes_the_next_one() {
        let dir = scratch("removed");
        let path = dir.join(".tree.lock");
        let opened = Opened::create(&path).unwrap();
        Turn::now(&path).unwrap().remove();
        let next = Turn::now(&path).expect("the next taker makes the file again");
        opened.file.lock().unwrap();
        assert!(
            opened.named().is_none(),
            "a lock on the removed file is no turn"
        );
        assert!(Turn::now(&path).is_none(), "the next one is held");
        drop(next);
        assert!(Turn::by(&path, Deadline::after(None)).unwrap().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_taker_waiting_on_a_turn_that_is_removed_ends_holding_the_file_at_its_path() {
        let dir = scratch("waiting");
        let path = dir.join(".tree.lock");
        let sweep = Turn::now(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = {
            let path = path.clone();
            std::thread::spawn(move || {
                let turn = Turn::by(&path, Deadline::after(None)).unwrap().unwrap();
                tx.send(()).unwrap();
                turn
            })
        };
        sweep.remove();
        rx.recv().unwrap();
        assert!(
            Turn::now(&path).is_none(),
            "the waiter holds the file at the path"
        );
        drop(waiter.join().unwrap());
        assert!(Turn::by(&path, Deadline::after(None)).unwrap().is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
