use crate::{
    clock::Moment,
    platform::{Host, Platform as _},
    tree::{
        base::LOCAL,
        builds::Builds,
        clocks::{Clocks, Contents as _, Dates as _, Fate, Removal, USED},
        runners::Runners,
        spread::Spread as _,
        turn::Turn,
    },
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// The lock a prepare holds over a repo's trees while it adds or revives one, and a sweep while
/// it removes one. Never removed: a bash prepare takes it without checking it names its file.
pub const PREPARE_LOCK: &str = ".prepare.lock";
/// What names a prepare's copy, or a tree it set aside, ahead of the prepare's stamp, whose first
/// number is the prepare's process.
const TEMPORARY: [&str; 2] = [".seed.", ".old."];
/// What a sweep writes in a cache it found unmarked; a prepare's marker is empty.
const DATED: &str = "swept\n";

/// What a sweep walks, in the order it walks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swept {
    Trees,
    Caches,
    Jobs,
    /// Under `tmp`.
    Leftovers,
    /// Under `out`, where a person keeps what a job made.
    Results,
    Runners,
}

impl Swept {
    /// What `dibs --gc` sweeps.
    pub const ALL: [Swept; 6] = [
        Swept::Trees,
        Swept::Caches,
        Swept::Jobs,
        Swept::Leftovers,
        Swept::Results,
        Swept::Runners,
    ];
    /// What a prepare sweeps: results go only when a person asks, with `dibs --gc`.
    pub const BY_PREPARE: [Swept; 5] = [
        Swept::Trees,
        Swept::Caches,
        Swept::Jobs,
        Swept::Leftovers,
        Swept::Runners,
    ];

    /// The section as the sweep's timing names it.
    pub fn named(self) -> &'static str {
        match self {
            Swept::Trees => "worktrees",
            Swept::Caches => "build caches",
            Swept::Jobs => "job logs",
            Swept::Leftovers => "leftovers",
            Swept::Results => "results",
            Swept::Runners => "runners",
        }
    }
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
    pub kind: Swept,
    pub verdicts: Vec<Verdict>,
    _lock: Option<File>,
}

impl Section {
    pub fn holding(kind: Swept, verdicts: Vec<Verdict>, lock: Option<File>) -> Section {
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
    /// Where running processes work, read once: past every clock, nothing else keeps a tree that
    /// a job's step is running in.
    in_use: OnceLock<Vec<PathBuf>>,
}

impl<'a> Sweep<'a> {
    pub fn new(scratch: &'a Path, home: &'a Path, clocks: Clocks, removal: Removal<'a>) -> Self {
        Sweep {
            scratch,
            home,
            clocks,
            removal,
            dry: false,
            in_use: OnceLock::new(),
        }
    }

    /// A process works in it, or builds into it: what a job's step holds only by running there.
    fn used_now(&self, path: &Path) -> bool {
        let Ok(path) = path.canonicalize() else {
            return false;
        };
        self.in_use
            .get_or_init(|| {
                Host::processes()
                    .iter()
                    .flat_map(|p| {
                        let target = Host::variable(p.pid, "CARGO_TARGET_DIR").map(PathBuf::from);
                        [Host::cwd(p.pid), target]
                    })
                    .flatten()
                    .filter_map(|p| p.canonicalize().ok())
                    .collect()
            })
            .iter()
            .any(|used| used.starts_with(&path))
    }

    /// One that judges and removes nothing.
    pub fn dry(self) -> Self {
        Sweep { dry: true, ..self }
    }

    /// What a prepare sweeps judged, and what is past its clock removed.
    pub fn run(&self) {
        let now = Moment::epoch_now();
        for kind in Swept::BY_PREPARE {
            if let Some(mut section) = self.judged(kind, now) {
                self.collect(&mut section, now);
            }
        }
    }

    /// What there is of one kind, before anything is judged, which dates what has no marker.
    pub fn entries(&self, kind: Swept) -> Vec<PathBuf> {
        let under = |dir: &str| self.scratch.join(dir).entries();
        match kind {
            Swept::Trees => under("ws")
                .iter()
                .flat_map(|repo| repo.entries())
                .filter(|tree| tree.is_dir())
                .collect(),
            Swept::Caches => under("target")
                .into_iter()
                .filter(|cache| cache.is_dir())
                .collect(),
            Swept::Jobs => under("jobs"),
            Swept::Leftovers => under("tmp"),
            Swept::Results => under("out"),
            Swept::Runners => Runners::in_home(self.home).dir().entries(),
        }
    }

    /// One kind judged; None for the runners while one of them builds.
    pub fn judged(&self, kind: Swept, now: u64) -> Option<Section> {
        let entries = self.entries(kind).into_iter();
        let verdicts = match kind {
            Swept::Trees => entries
                .map(|tree| {
                    let fate = self.tree(&tree, now);
                    let used = tree.used(now);
                    Verdict::of(tree, fate, used)
                })
                .collect(),
            Swept::Caches => entries
                .map(|cache| {
                    let fate = self.cache(&cache, now);
                    let used = cache.used(now);
                    Verdict::of(cache, fate, used)
                })
                .collect(),
            Swept::Jobs | Swept::Leftovers | Swept::Results => entries
                .map(|path| {
                    let fate = self.bulk(&path, now);
                    let used = path.written(now);
                    Verdict::of(path, fate, used)
                })
                .collect(),
            Swept::Runners => return Runners::in_home(self.home).judged(&self.clocks, now),
        };
        Some(Section::holding(kind, verdicts, None))
    }

    /// What is past its clock in `section` judged again under the locks a prepare revives it
    /// under, and removed holding them unless the sweep is dry. Judging takes no lock, since
    /// one taken and let go can stay held a moment in a process another thread forks.
    pub fn collect(&self, section: &mut Section, now: u64) {
        self.collect_saying(section, now, &|_| {});
    }

    /// The same, with `gone` told each path once its removal was tried.
    pub fn collect_saying(&self, section: &mut Section, now: u64, gone: &(dyn Fn(&Path) + Sync)) {
        match section.kind {
            Swept::Trees => self.collect_trees(&mut section.verdicts, now, gone),
            // Set aside one at a time under their locks, then removed on every core: a cache of
            // seeded copies can take minutes to remove, and one does not wait on another.
            Swept::Caches => {
                let asides: Vec<(usize, PathBuf)> = section
                    .verdicts
                    .iter_mut()
                    .enumerate()
                    .filter_map(|(at, v)| Some((at, self.collect_cache(v, now)?)))
                    .collect();
                let paths: Vec<(PathBuf, PathBuf)> = asides
                    .iter()
                    .map(|(at, aside)| (section.verdicts[*at].path.clone(), aside.clone()))
                    .collect();
                let until = self.removal.until();
                let went = paths.spread(|(path, aside)| {
                    let went = aside.removed_by(until);
                    gone(path);
                    went
                });
                for ((at, aside), went) in asides.iter().zip(went) {
                    // One a gone prepare left set aside kept its lock, which goes with it now.
                    if went && *aside == section.verdicts[*at].path {
                        let _ = fs::remove_file(Turn::beside(aside));
                    }
                    section.verdicts[*at].removed = self.removal.removed(aside, went);
                }
            }
            Swept::Jobs | Swept::Leftovers | Swept::Results | Swept::Runners if self.dry => {}
            Swept::Jobs | Swept::Leftovers | Swept::Results | Swept::Runners => {
                for verdict in section.verdicts.iter_mut().filter(|v| v.fate == Fate::Past) {
                    verdict.removed = self.removal.path(&verdict.path);
                    gone(&verdict.path);
                }
            }
        }
    }

    /// Each repo's past trees under its prepare lock, judged again there, and the clone they were
    /// added from pruned of them.
    fn collect_trees(&self, verdicts: &mut [Verdict], now: u64, gone: &(dyn Fn(&Path) + Sync)) {
        let mut repos: BTreeMap<PathBuf, Vec<&mut Verdict>> = BTreeMap::new();
        for verdict in verdicts.iter_mut().filter(|v| v.fate == Fate::Past) {
            let repo = verdict.path.parent().unwrap_or(self.scratch).to_path_buf();
            repos.entry(repo).or_default().push(verdict);
        }
        for (repo, past) in repos {
            let Some(_lock) = Turn::now(&repo.join(PREPARE_LOCK)) else {
                past.into_iter().for_each(|v| v.fate = Fate::Preparing);
                continue;
            };
            let mut pruned = false;
            for verdict in past {
                verdict.fate = self.tree(&verdict.path, now);
                if verdict.fate != Fate::Past || self.dry {
                    continue;
                }
                let Some(turns) = Sweep::sent_turns(&repo, &verdict.path) else {
                    verdict.fate = Fate::Preparing;
                    continue;
                };
                verdict.removed = self.removal.tree(&verdict.path);
                gone(&verdict.path);
                pruned = true;
                turns
                    .into_iter()
                    .for_each(|turn| turn.remove_with(&verdict.path));
            }
            if pruned && let Some(name) = repo.file_name() {
                self.removal.prune(&self.home.join("prog").join(name));
            }
        }
    }

    /// A past cache under the lock a prepare revives it under, with every build lock in it held,
    /// judged again there and set aside, and the lock removed with it: where it went, to remove.
    fn collect_cache(&self, verdict: &mut Verdict, now: u64) -> Option<PathBuf> {
        if !matches!(verdict.fate, Fate::Past | Fate::Hollow) || self.removal.spent() {
            return None;
        }
        let Some(turn) = Turn::now(&Turn::beside(&verdict.path)) else {
            verdict.fate = Fate::Preparing;
            return None;
        };
        let Some(builds) = Builds::new(&verdict.path).all_exclusive() else {
            verdict.fate = Fate::Held;
            return None;
        };
        verdict.fate = self.cache(&verdict.path, now);
        if self.dry {
            return None;
        }
        let mut aside = None;
        match verdict.fate {
            Fate::Past => {
                let to = match Sweep::temporary(&verdict.path) {
                    Some(_) => verdict.path.clone(),
                    None => PathBuf::from(format!(
                        "{}.old.{}-swept",
                        verdict.path.display(),
                        std::process::id()
                    )),
                };
                if self.removal.set_aside(&verdict.path, &to) {
                    aside = Some(to);
                }
            }
            Fate::Hollow => self.removal.hollow(&verdict.path),
            _ => {}
        }
        drop(builds);
        turn.remove_with(&verdict.path);
        aside
    }

    /// The turns of the sent trees `tree` is or nests, each taken at once; None when a prepare
    /// holds one. Only a turn whose file is there can be held.
    fn sent_turns(trees: &Path, tree: &Path) -> Option<Vec<Turn>> {
        let name = tree.file_name().unwrap_or_default().to_string_lossy();
        let turns = match name.starts_with(LOCAL) {
            true => vec![Turn::of_sent(trees, &name, None)],
            false => tree
                .entries()
                .iter()
                .filter_map(|sent| sent.file_name()?.to_str().map(str::to_string))
                .filter(|sent| sent.starts_with(LOCAL))
                .map(|sent| Turn::of_sent(trees, &sent, Some(&name)))
                .collect(),
        };
        turns
            .iter()
            .filter(|turn| turn.exists())
            .map(|turn| Turn::now(turn))
            .collect()
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
        if self.clocks.all && self.used_now(tree) {
            return Fate::Held;
        }
        match used.past(now, &self.clocks, self.clocks.keep_days) {
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
        if self.clocks.all && self.used_now(cache) {
            return Fate::Held;
        }
        match used.past(now, &self.clocks, self.clocks.target_keep_days) {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

    /// A job's directory, or a leftover file, by its own time.
    fn bulk(&self, path: &Path, now: u64) -> Fate {
        let written = fs::symlink_metadata(path).map(|_| path.written(now));
        match written.is_ok_and(|at| self.clocks.past(now, at, self.clocks.keep_days)) {
            true => Fate::Past,
            false => Fate::Kept,
        }
    }

    /// A cache the sweep may judge past its clock, read without its lock and without dating an
    /// undated one: a superset of what goes, since a build may hold one by then.
    pub fn may_go(&self, cache: &Path, now: u64) -> bool {
        match Sweep::temporary(cache) {
            Some(fate) => fate == Fate::Past,
            None => cache
                .join(USED)
                .past(now, &self.clocks, self.clocks.target_keep_days),
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
