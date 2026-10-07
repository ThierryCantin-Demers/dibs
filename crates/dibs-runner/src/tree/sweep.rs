use crate::{
    clock::Moment,
    platform::{Host, Platform as _},
    tree::{
        builds::{Builds, FileLock},
        clocks::{Clocks, Contents as _, Dates as _, Fate, Removal, USED},
        copy::Coreutils as _,
        runners::Runners,
    },
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
};

/// The lock a prepare holds over a repo's trees while it adds or revives one, and a sweep while
/// it removes one.
pub const PREPARE_LOCK: &str = ".prepare.lock";
/// What names a prepare's copy, or a tree it set aside, ahead of the prepare's stamp, whose first
/// number is the prepare's process.
const TEMPORARY: [&str; 2] = [".seed.", ".old."];
/// What a sweep writes in a cache it found unmarked; a prepare's marker is empty.
const DATED: &str = "swept\n";

/// What a sweep walks, in the order it walks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Trees,
    Caches,
    Jobs,
    Leftovers,
    Runners,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Trees,
        Kind::Caches,
        Kind::Jobs,
        Kind::Leftovers,
        Kind::Runners,
    ];
}

/// One entry as its clock judged it, and whether the sweep removed it.
pub struct Verdict {
    pub path: PathBuf,
    pub fate: Fate,
    /// When it was last used.
    pub used: u64,
    pub removed: bool,
}

impl Verdict {
    pub fn of(path: PathBuf, fate: Fate, used: u64) -> Verdict {
        Verdict {
            path,
            fate,
            used,
            removed: false,
        }
    }
}

/// One kind's entries as judged, with whatever lock keeps them judged until what is past them
/// is gone.
pub struct Section {
    pub kind: Kind,
    pub verdicts: Vec<Verdict>,
    _lock: Option<File>,
}

impl Section {
    pub fn holding(kind: Kind, verdicts: Vec<Verdict>, lock: Option<File>) -> Section {
        Section {
            kind,
            verdicts,
            _lock: lock,
        }
    }
}

/// One walk of a scratch directory, which every prepare runs over every repo and `dibs --gc`
/// reports: most runs are local, and a repo nobody prepares any more would otherwise never be
/// swept. What is past its clock is judged again under the locks a prepare takes to revive it,
/// which are held until it is gone.
pub struct Sweep<'a> {
    scratch: &'a Path,
    home: &'a Path,
    clocks: Clocks,
    removal: Removal<'a>,
    dry: bool,
}

impl<'a> Sweep<'a> {
    pub fn new(scratch: &'a Path, home: &'a Path, clocks: Clocks, removal: Removal<'a>) -> Self {
        Sweep {
            scratch,
            home,
            clocks,
            removal,
            dry: false,
        }
    }

    /// One that judges and removes nothing.
    pub fn dry(self) -> Self {
        Sweep { dry: true, ..self }
    }

    /// Every kind judged, and what is past its clock removed.
    pub fn run(&self) {
        let now = Moment::epoch_now();
        for kind in Kind::ALL {
            if let Some(mut section) = self.judged(kind, now) {
                self.collect(&mut section, now);
            }
        }
    }

    /// What there is of one kind, before anything is judged, which dates what has no marker.
    pub fn entries(&self, kind: Kind) -> Vec<PathBuf> {
        let under = |dir: &str| self.scratch.join(dir).entries();
        match kind {
            Kind::Trees => under("ws")
                .iter()
                .flat_map(|repo| repo.entries())
                .filter(|tree| tree.is_dir())
                .collect(),
            Kind::Caches => under("target")
                .into_iter()
                .filter(|cache| cache.is_dir())
                .collect(),
            Kind::Jobs => under("jobs"),
            Kind::Leftovers => [under("tmp"), under("out")].concat(),
            Kind::Runners => Runners::in_home(self.home).dir().entries(),
        }
    }

    /// One kind judged; None for the runners while one of them builds.
    pub fn judged(&self, kind: Kind, now: u64) -> Option<Section> {
        let entries = self.entries(kind).into_iter();
        let verdicts = match kind {
            Kind::Trees => entries
                .map(|tree| {
                    let fate = self.tree(&tree, now);
                    let used = tree.used(now);
                    Verdict::of(tree, fate, used)
                })
                .collect(),
            Kind::Caches => entries
                .map(|cache| {
                    let fate = self.cache(&cache, now);
                    let used = cache.used(now);
                    Verdict::of(cache, fate, used)
                })
                .collect(),
            Kind::Jobs | Kind::Leftovers => entries
                .map(|path| {
                    let fate = self.bulk(&path, now);
                    let used = path.written(now);
                    Verdict::of(path, fate, used)
                })
                .collect(),
            Kind::Runners => return Runners::in_home(self.home).judged(&self.clocks, now),
        };
        Some(Section::holding(kind, verdicts, None))
    }

    /// What is past its clock in `section` judged again under the locks a prepare revives it
    /// under, and removed holding them unless the sweep is dry. Judging takes no lock, since
    /// one taken and let go can stay held a moment in a process another thread forks.
    pub fn collect(&self, section: &mut Section, now: u64) {
        match section.kind {
            Kind::Trees => self.collect_trees(&mut section.verdicts, now),
            Kind::Caches => {
                for verdict in &mut section.verdicts {
                    self.collect_cache(verdict, now);
                }
            }
            Kind::Jobs | Kind::Leftovers | Kind::Runners if self.dry => {}
            Kind::Jobs | Kind::Leftovers | Kind::Runners => {
                for verdict in section.verdicts.iter_mut().filter(|v| v.fate == Fate::Past) {
                    verdict.removed = self.removal.path(&verdict.path);
                }
            }
        }
    }

    /// Each repo's past trees under its prepare lock, judged again there, and the clone they were
    /// added from pruned of them.
    fn collect_trees(&self, verdicts: &mut [Verdict], now: u64) {
        let mut repos: BTreeMap<PathBuf, Vec<&mut Verdict>> = BTreeMap::new();
        for verdict in verdicts.iter_mut().filter(|v| v.fate == Fate::Past) {
            let repo = verdict.path.parent().unwrap_or(self.scratch).to_path_buf();
            repos.entry(repo).or_default().push(verdict);
        }
        for (repo, past) in repos {
            let Some(_lock) = FileLock::exclusive_now(&repo.join(PREPARE_LOCK)) else {
                past.into_iter().for_each(|v| v.fate = Fate::Preparing);
                continue;
            };
            let mut pruned = false;
            for verdict in past {
                verdict.fate = self.tree(&verdict.path, now);
                if verdict.fate == Fate::Past && !self.dry {
                    verdict.removed = self.removal.tree(&verdict.path);
                    pruned = true;
                }
            }
            if pruned && let Some(name) = repo.file_name() {
                self.removal.prune(&self.home.join("prog").join(name));
            }
        }
    }

    /// A past cache under the lock a prepare revives it under, with every build lock in it held,
    /// judged again there.
    fn collect_cache(&self, verdict: &mut Verdict, now: u64) {
        if !matches!(verdict.fate, Fate::Past | Fate::Hollow) {
            return;
        }
        let Some(_turn) = FileLock::exclusive_now(&FileLock::beside(&verdict.path)) else {
            verdict.fate = Fate::Preparing;
            return;
        };
        let Some(_builds) = Builds::new(&verdict.path).all_exclusive() else {
            verdict.fate = Fate::Held;
            return;
        };
        verdict.fate = self.cache(&verdict.path, now);
        if self.dry {
            return;
        }
        match verdict.fate {
            Fate::Past => verdict.removed = self.removal.path(&verdict.path),
            Fate::Hollow => self.removal.hollow(&verdict.path),
            _ => {}
        }
    }

    fn tree(&self, tree: &Path, now: u64) -> Fate {
        if let Some(fate) = Sweep::temporary(tree) {
            return fate;
        }
        let used = tree.join(USED);
        if !used.exists() {
            let _ = used.touch();
            return Fate::Dated;
        }
        match used.unchanged_for(now, self.clocks.keep_days) {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

    /// By its marker alone: whether a build holds it is for the sweep to ask, holding its locks.
    fn cache(&self, cache: &Path, now: u64) -> Fate {
        if let Some(fate) = Sweep::temporary(cache) {
            return fate;
        }
        let used = cache.join(USED);
        let marker = fs::metadata(&used).ok();
        let alone = fs::read_dir(cache)
            .map(|d| d.flatten().all(|e| e.file_name() == USED))
            .unwrap_or(false);
        if marker.as_ref().is_some_and(|m| m.is_file() && m.len() > 0) && alone {
            return Fate::Hollow;
        }
        if marker.is_none() {
            let _ = fs::write(&used, DATED);
            return Fate::Dated;
        }
        match used.unchanged_for(now, self.clocks.target_keep_days) {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

    /// A job's directory, or a leftover file, by its own time.
    fn bulk(&self, path: &Path, now: u64) -> Fate {
        let written = fs::symlink_metadata(path).map(|_| path.written(now));
        match written.is_ok_and(|at| Clocks::days(now, at) > self.clocks.keep_days) {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

    /// A prepare's copy or set-aside tree: its own while the prepare runs, and past once it has
    /// gone without cleaning up.
    fn temporary(path: &Path) -> Option<Fate> {
        let name = path.file_name()?.to_str()?;
        let stamp = TEMPORARY
            .iter()
            .find_map(|mark| name.rsplit_once(mark).map(|(_, stamp)| stamp))?;
        let pid: u32 = stamp.split('-').next()?.parse().ok()?;
        Some(match Host::exists(pid) {
            true => Fate::Preparing,
            false => Fate::Past,
        })
    }
}
