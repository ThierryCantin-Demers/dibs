use crate::tree::{
    base::Stamp,
    builds::{Builds, FileLock},
    clocks::USED,
    copy::{Copier, Coreutils as _, Sharing},
    git::Unstopped,
    packages::{Cache, Lines},
};
use dibs_format::wire::{Seeded, Shared};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// A new tree starts from a reflink copy of a sibling's target directory, and of that sibling's
/// sources when it has some, since trees of one repo differ in a few files. The sibling is the
/// one that has built the most of this tree's lockfile, newest first among equals: cargo reuses a
/// crate only at the same version and git revision, so the newest sibling on another revision
/// saves little.
///
/// A sibling a build holds is skipped, since cargo writes artifacts before their fingerprints,
/// unless that build will leave it with more of this lockfile than anything idle has: trees moved
/// to a new dependency at once would otherwise each build it. That one is waited for, and taken if
/// it succeeded. A filesystem that cannot share blocks gets no seed at all.
pub struct Seed<'a> {
    tree: Seedling<'a>,
    /// The least a sibling must have built of the tree's lockfile to be taken at all.
    floor: u64,
    wait: Duration,
    copier: Copier,
    say: &'a dyn Fn(&str),
}

/// The tree a seed is for: where it goes, its lockfile, and what its sources start without.
pub struct Seedling<'a> {
    pub scratch: &'a Path,
    pub repo: &'a str,
    pub target: &'a Path,
    pub worktree: &'a Path,
    /// This tree's lockfile, when it has one.
    pub packages: Option<&'a Lines>,
    /// Paths the copied sources start without.
    pub fresh: &'a [String],
    pub stamp: &'a Stamp,
}

/// What a seed does where the tree or its target already exists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Existing {
    /// Left alone: the seed stops, or comes without sources.
    Kept,
    /// To be replaced, so the copy is made anyway.
    Replaced,
}

/// A sibling target, ranked by what it has built of this tree's lockfile, or will have once the
/// build running there succeeds.
struct Sibling {
    rank: u64,
    have: u64,
    building: bool,
    dir: PathBuf,
}

/// A sibling's locks as far as they could be taken.
struct Taken {
    #[allow(dead_code, reason = "held for their locks")]
    locks: Vec<FileLock>,
    /// Every one was taken.
    free: bool,
    /// One was waited for.
    waited: bool,
}

/// A sibling's target copied beside this tree's, with the tree claimed in it, and its sources
/// when they came too: nothing of the tree itself has moved yet.
pub struct Copied {
    target: PathBuf,
    sources: Option<PathBuf>,
    seeded: Seeded,
}

impl<'a> Seed<'a> {
    pub fn new(
        tree: Seedling<'a>,
        floor: u64,
        wait: Duration,
        copier: Copier,
        say: &'a dyn Fn(&str),
    ) -> Self {
        Seed {
            tree,
            floor,
            wait,
            copier,
            say,
        }
    }

    /// A new tree's target, and its sources when the sibling has some.
    pub fn run(&self, unstopped: Unstopped) -> Option<Seeded> {
        self.copy(Existing::Kept)?
            .place(self.tree.target, self.tree.worktree, unstopped)
    }

    /// A copy to replace the tree and target that exist, which the caller places or discards.
    pub fn replacement(&self) -> Option<Copied> {
        self.copy(Existing::Replaced)
    }

    /// The best sibling's target copied, chosen before anything of this tree moves.
    fn copy(&self, existing: Existing) -> Option<Copied> {
        let siblings = self.siblings();
        let idle_best = siblings
            .iter()
            .filter(|s| !s.building)
            .map(|s| s.have)
            .max()
            .unwrap_or(0);
        let copy = suffixed(
            self.tree.target,
            &format!(".seed.{}", self.tree.stamp.as_str()),
        );
        for sibling in siblings {
            if sibling.rank < self.floor || self.tree.target.is_dir() && existing == Existing::Kept
            {
                break;
            }
            let mut have = sibling.have;
            let taken = self.hold(&sibling, idle_best);
            let mut held = !taken.free;
            if taken.waited {
                if let Some(record) = Cache::new(&sibling.dir).record() {
                    have = self.shared(&record);
                }
                held |= have < idle_best;
            }
            held |= have < self.floor;
            let copied = !held
                && self
                    .copier
                    .tree(&sibling.dir, &copy, Sharing::Required)
                    .is_ok()
                && fs::write(
                    copy.join(".dibs-tree"),
                    format!("{}\n", self.tree.worktree.display()),
                )
                .is_ok()
                && copy.join(USED).make_empty().is_ok();
            drop(taken);
            if !copied {
                copy.remove_all();
                continue;
            }
            let from = name(&sibling.dir);
            return Some(Copied {
                sources: self.sources(&from, existing),
                seeded: Seeded {
                    shared: self.tree.packages.map(|p| Shared { have, of: p.len() }),
                    sources: false,
                    from,
                },
                target: copy,
            });
        }
        None
    }

    /// The candidates `ls -t` names, newest first, ranked with `sort -s`: a stable sort keeps
    /// the newest first among equals.
    fn siblings(&self) -> Vec<Sibling> {
        let targets = self.tree.scratch.join("target");
        let local = format!("{}-local-", self.tree.repo);
        let arm = format!("{}-arm", self.tree.repo);
        let mut found: Vec<(SystemTime, PathBuf)> = fs::read_dir(&targets)
            .map(|d| {
                d.flatten()
                    .filter(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        name == self.tree.repo || name.starts_with(&local) || name.starts_with(&arm)
                    })
                    .filter_map(|e| {
                        let used = fs::metadata(e.path().join(".dibs-used")).ok()?;
                        Some((used.modified().ok()?, e.path()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let mut ranked: Vec<Sibling> = found
            .into_iter()
            .map(|(_, dir)| dir)
            .filter(|dir| {
                let shown = dir.display().to_string();
                dir != self.tree.target && !shown.contains(".old.") && !shown.contains(".seed.")
            })
            .map(|dir| self.ranked(dir))
            .collect();
        ranked.sort_by_key(|s| std::cmp::Reverse(s.rank));
        ranked
    }

    fn ranked(&self, dir: PathBuf) -> Sibling {
        let building = Builds::new(&dir).running();
        let cache = Cache::new(&dir);
        let (have, will) = match self.tree.packages {
            Some(_) => (
                cache.record().map_or(0, |record| self.shared(&record)),
                match building {
                    true => self.shared(&cache.after_its_build()),
                    false => 0,
                },
            ),
            None => (0, 0),
        };
        Sibling {
            rank: will.max(have),
            have,
            building,
            dir,
        }
    }

    fn shared(&self, other: &Lines) -> u64 {
        self.tree.packages.map_or(0, |p| p.shared_with(other))
    }

    /// The sibling's locks held shared, so no build starts in it while it is copied; a build
    /// that holds one is waited for when it will leave more of this lockfile than anything idle
    /// has.
    fn hold(&self, sibling: &Sibling, idle_best: u64) -> Taken {
        let mut taken = Taken {
            locks: Vec::new(),
            free: false,
            waited: false,
        };
        for lock in Builds::new(&sibling.dir).locks() {
            if let Some(held) = FileLock::shared_now(&lock) {
                taken.locks.push(held);
                continue;
            }
            if sibling.rank <= sibling.have || sibling.rank <= idle_best {
                return taken;
            }
            (self.say)(&format!(
                "dibs: waiting up to {}s for the build in {}, which will have {} of this tree's lockfile groups where anything idle has {idle_best}\n",
                self.wait.as_secs(),
                name(&sibling.dir),
                sibling.rank
            ));
            match FileLock::shared_within(&lock, self.wait) {
                Some(held) => {
                    taken.locks.push(held);
                    taken.waited = true;
                }
                None => return taken,
            }
        }
        taken.free = true;
        taken
    }

    /// The sibling's sources copied, when it is a local tree's and this tree's own are new or
    /// being replaced: a fetched tree has its own.
    fn sources(&self, from: &str, existing: Existing) -> Option<PathBuf> {
        let key = from.strip_prefix(&format!("{}-local-", self.tree.repo))?;
        let sources = self
            .tree
            .scratch
            .join("ws")
            .join(self.tree.repo)
            .join(format!("local-{key}"));
        if self.tree.worktree.exists() && existing == Existing::Kept || !sources.is_dir() {
            return None;
        }
        let copy = suffixed(
            self.tree.worktree,
            &format!(".seed.{}", self.tree.stamp.as_str()),
        );
        // `rm -rf` of each path: every one is tried, and all must go.
        let copied = self
            .copier
            .tree(&sources, &copy, Sharing::Preferred)
            .is_ok()
            && {
                let gone: Vec<bool> = self
                    .tree
                    .fresh
                    .iter()
                    .map(|path| Path::new(&format!("{}/{path}", copy.display())).remove_all())
                    .collect();
                gone.iter().all(|g| *g)
            };
        if !copied {
            copy.remove_all();
        }
        copied.then_some(copy)
    }
}

impl Copied {
    /// In place of a tree that has no target yet; its sources only where it has none either.
    pub fn place(mut self, target: &Path, worktree: &Path, unstopped: Unstopped) -> Option<Seeded> {
        let mut placed = false;
        unstopped(&mut || {
            placed = fs::rename(&self.target, target).is_ok();
            if placed && let Some(sources) = &self.sources {
                self.seeded.sources = !worktree.exists() && fs::rename(sources, worktree).is_ok();
            }
        });
        if !placed {
            self.discard();
            return None;
        }
        if let Some(sources) = &self.sources {
            sources.remove_all();
        }
        Some(self.seeded)
    }

    /// In place of an existing tree and its target, which go. Its sources go even where the
    /// sibling's did not come, since files dated from one tree and compared against another's
    /// artifacts could pass for fresh: the sync after sends them all again.
    pub fn replace(
        mut self,
        target: &Path,
        worktree: &Path,
        stamp: &Stamp,
        say: &dyn Fn(&str),
        unstopped: Unstopped,
    ) -> Option<Seeded> {
        let aside = format!(".old.{}", stamp.as_str());
        let (old_target, old_tree) = (suffixed(target, &aside), suffixed(worktree, &aside));
        let mut replaced = false;
        unstopped(&mut || replaced = self.swap(target, worktree, &old_target, &old_tree));
        if !replaced {
            self.discard();
            return None;
        }
        if let Some(sources) = &self.sources {
            sources.remove_all();
        }
        if !(old_target.remove_all() & old_tree.remove_all()) {
            say(
                "dibs: could not remove all of what the reseed replaced; the next sweep takes it\n",
            );
        }
        Some(self.seeded)
    }

    /// The tree and its target set aside and the copies moved in, or, where a rename fails,
    /// everything moved back.
    fn swap(&mut self, target: &Path, worktree: &Path, old_target: &Path, old_tree: &Path) -> bool {
        if fs::rename(worktree, old_tree).is_err() {
            return false;
        }
        if fs::rename(target, old_target).is_err() {
            let _ = fs::rename(old_tree, worktree);
            return false;
        }
        if fs::rename(&self.target, target).is_err() {
            let _ = fs::rename(old_target, target);
            let _ = fs::rename(old_tree, worktree);
            return false;
        }
        if let Some(sources) = &self.sources {
            self.seeded.sources = fs::rename(sources, worktree).is_ok();
        }
        true
    }

    pub fn discard(&self) {
        self.target.remove_all();
        if let Some(sources) = &self.sources {
            sources.remove_all();
        }
    }
}

/// A path's last component, as `${path##*/}` reads it.
fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `path` with `suffix` added to its last component.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", path.display()))
}
