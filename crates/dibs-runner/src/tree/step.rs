use crate::tree::{
    clocks::Contents as _,
    copy::{Coreutils as _, Mark},
    glob::Glob,
    packages::{Cache, Lines, RECORD},
};
use dibs_format::{Exit, lockfile::Package};
use std::{
    fs::{self, File},
    os::fd::{AsFd as _, BorrowedFd},
    path::{Path, PathBuf},
};

/// Where a target records the tree whose build it last held.
const CLAIM: &str = ".dibs-tree";
/// The job's command, whose time is when the job started.
const STARTED: &str = "cmd";
/// What a pattern under the target starts with, as a recipe writes it.
const UNDER_TARGET: &str = "$CARGO_TARGET_DIR/";
/// What a build leaves in its target while it runs, ahead of the runner's pid.
const BUILDING: &str = ".dibs-building.";
/// What rustc and cargo keep per crate, under `<profile>` and `<triple>/<profile>`.
const PER_CRATE: [&str; 2] = ["incremental", ".fingerprint"];
/// A status above this is a signal's: 128 and its number.
const SIGNALLED: i32 = 128;

/// A build's mark in its target, held shared by the runner and by the build's own processes: one
/// nobody holds was left by a build stopped partway, whose rustc can outlive its runner.
pub struct BuildMark {
    mark: PathBuf,
    held: File,
}

impl BuildMark {
    /// The lock, for the build's own processes to hold.
    pub fn held(&self) -> BorrowedFd<'_> {
        self.held.as_fd()
    }

    /// A build that ended on its own takes its mark away; one stopped, at its cap or by a signal,
    /// leaves it for the next build to find unheld.
    pub fn ended(self, status: i32) {
        if status != Exit::Overran.status() && status <= SIGNALLED {
            let _ = fs::remove_file(&self.mark);
        }
    }
}

/// Where a recipe step runs: its tree, and the target it builds into.
pub struct Spot {
    pub worktree: PathBuf,
    pub target: PathBuf,
}

/// A recipe step's command in its tree: what is done before it and after it.
pub struct Stepping<'a> {
    worktree: &'a Path,
    target: &'a Path,
    /// Where the job keeps its log, and what the step keeps beside it.
    job_dir: Option<&'a Path>,
    say: &'a dyn Fn(&str),
}

impl<'a> Stepping<'a> {
    pub fn new(spot: &'a Spot, job_dir: Option<&'a Path>, say: &'a dyn Fn(&str)) -> Self {
        Stepping {
            worktree: &spot.worktree,
            target: &spot.target,
            job_dir,
            say,
        }
    }

    /// A measurement refused because another tree built into the target after this one did:
    /// the binary there may be that tree's. Read under the exclusive lock, so nothing builds in
    /// between.
    pub fn refused(&self) -> bool {
        if self.claimed() {
            return false;
        }
        (self.say)(&format!(
            "dibs: refused to measure: another tree built into {} after this one did, so the binary there may be that tree's.\n  \
             Run it again, which rebuilds this tree first, or pass --anyway to measure what is there.\n",
            self.target.display()
        ));
        true
    }

    /// The target claimed for this tree. Cargo judges a crate fresh when its sources are older
    /// than its last compile, so a tree checked out before another tree built into a shared
    /// target is handed that tree's artifacts unless its sources are dated after them. The job's
    /// start is marked again after them, or `keep` would take every file of the tree for new.
    pub fn claim(&self) {
        let lock = File::create(self.target.join(format!("{CLAIM}.lock")));
        let _held = lock.and_then(|lock| lock.lock().map(|()| lock));
        if self.claimed() {
            return;
        }
        dated_now(self.worktree);
        if let Some(job_dir) = self.job_dir {
            let _ = Mark(&job_dir.join(STARTED)).set();
        }
        let _ = fs::write(
            self.target.join(CLAIM),
            format!("{}\n", self.worktree.display()),
        );
        (self.say)(&format!(
            "dibs: this tree did not make the last build in {}, so cargo rebuilds its crates\n",
            self.target.display()
        ));
    }

    /// This build's mark, after what a build stopped partway touched is put out of reach: rustc
    /// reuses a stopped session's incremental state, and the result links with symbols missing,
    /// so those crates are built again from nothing.
    pub fn building(&self) -> Option<BuildMark> {
        for mark in self.target.entries() {
            let stopped = mark
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(BUILDING));
            if stopped
                && let Ok(unheld) = File::open(&mark)
                && unheld.try_lock().is_ok()
            {
                self.forget_since(&mark);
            }
        }
        let mark = self
            .target
            .join(format!("{BUILDING}{}", std::process::id()));
        let file = File::create(&mark).ok()?;
        file.lock_shared().ok()?;
        Some(BuildMark { mark, held: file })
    }

    /// What each crate kept that is newer than a stopped build's mark, removed, and the mark.
    fn forget_since(&self, mark: &Path) {
        let Ok(since) = fs::metadata(mark).and_then(|m| m.modified()) else {
            return;
        };
        let profiles = self.target.entries().into_iter().flat_map(|top| {
            let mut below = top.entries();
            below.push(top);
            below
        });
        for kept in profiles.flat_map(|profile| PER_CRATE.map(|dir| profile.join(dir))) {
            for entry in kept.entries() {
                if fs::metadata(&entry)
                    .and_then(|m| m.modified())
                    .is_ok_and(|at| at > since)
                {
                    entry.remove_all();
                }
            }
        }
        let _ = fs::remove_file(mark);
        (self.say)(&format!(
            "dibs: a build in {} was stopped partway, so the crates it touched are built again\n",
            self.target.display()
        ));
    }

    fn claimed(&self) -> bool {
        fs::read_to_string(self.target.join(CLAIM))
            .is_ok_and(|claim| claim.trim_end_matches('\n') == self.worktree.display().to_string())
    }

    /// What this build's own prepare staged joins the target's record, which is a union: old
    /// artifacts stay when a tree moves on, so a revision built last week still counts. The
    /// lock is for two builds of one target finishing together.
    pub fn record(&self, token: &str) {
        let cache = Cache::new(self.target);
        let staged = cache.staged_by(token);
        let Some(lines) = Lines::read(&staged).filter(|l| !l.is_empty()) else {
            return;
        };
        let lock = File::create(self.target.join(format!("{RECORD}.lock")));
        let _held = lock.and_then(|lock| lock.lock().map(|()| lock));
        let mut all = cache.record().unwrap_or_default();
        all.extend(lines);
        let written = self
            .target
            .join(format!("{RECORD}.new.{}", std::process::id()));
        if fs::write(&written, all.text()).is_ok()
            && fs::rename(&written, self.target.join(RECORD)).is_ok()
        {
            let _ = fs::remove_file(staged);
        }
    }

    /// The files the step wrote that `patterns` name, copied beside its log at their path in the
    /// tree, or under `target/` for one under the target. Only files newer than the job are
    /// taken: a tree is reused from run to run, and a file an earlier run left would look current.
    pub fn keep(&self, patterns: &[String]) -> Option<u32> {
        let job_dir = self.job_dir?;
        let started = fs::metadata(job_dir.join(STARTED))
            .and_then(|m| m.modified())
            .ok()?;
        let mut kept = 0;
        for pattern in patterns {
            let (base, shown) = match pattern.strip_prefix(UNDER_TARGET) {
                Some(rest) => (self.target, Some(rest)),
                None => (self.worktree, None),
            };
            for found in (Glob {
                pattern: shown.unwrap_or(pattern),
            })
            .under(base)
            {
                let from = base.join(&found);
                let new = fs::metadata(&from)
                    .is_ok_and(|m| m.is_file() && m.modified().is_ok_and(|at| at > started));
                if !new {
                    continue;
                }
                let at = match shown {
                    Some(_) => PathBuf::from("target").join(&found),
                    None => found,
                };
                let to = job_dir.join("artifacts").join(&at);
                let copied = to
                    .parent()
                    .map_or(Ok(()), fs::create_dir_all)
                    .and_then(|()| fs::copy(&from, &to))
                    .and_then(|_| fs::metadata(&from))
                    .and_then(|m| to.date_like(&m));
                kept += u32::from(copied.is_ok());
            }
        }
        (kept > 0).then_some(kept)
    }

    /// Whether a crate a pin replaces still comes from where it came before, said when it does:
    /// most often the pinned tree's version does not meet the requirement the dependency states.
    pub fn unpinned(&self, names: &[String]) -> bool {
        let text = fs::read_to_string(self.worktree.join("Cargo.lock")).unwrap_or_default();
        let left: Vec<String> = Package::all(&text)
            .into_iter()
            .filter(|p| names.contains(&p.name))
            .filter_map(|p| Some(format!("  {} from {}\n", p.name, p.source?)))
            .collect();
        if left.is_empty() {
            return false;
        }
        (self.say)(&format!(
            "dibs: the pin did not take. cargo still builds these from where they came before:\n{}  \
             Most often the pinned tree's version does not meet the requirement the dependency states.\n",
            left.concat()
        ));
        true
    }
}

/// Every file of a tree dated now, `.git` aside, as `touch -c` on each would.
fn dated_now(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() == ".git" {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => dated_now(&entry.path()),
            Ok(kind) if kind.is_file() => {
                let _ = entry.path().touch_existing();
            }
            _ => {}
        }
    }
}
