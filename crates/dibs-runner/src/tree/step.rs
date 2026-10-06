use crate::tree::{
    copy::{Mark, dated, now},
    glob::Glob,
    packages::{Cache, Lines, RECORD},
};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

/// Where a target records the tree whose build it last held.
const CLAIM: &str = ".dibs-tree";
/// The job's command, whose time is when the job started.
const STARTED: &str = "cmd";
/// What a pattern under the target starts with, as a recipe writes it.
const UNDER_TARGET: &str = "$CARGO_TARGET_DIR/";

/// A recipe step's command in its tree: what is done before it and after it.
pub struct Stepping<'a> {
    pub worktree: &'a Path,
    pub target: &'a Path,
    /// Where the job keeps its log, and what the step keeps beside it.
    pub job_dir: Option<&'a Path>,
    pub say: &'a dyn Fn(&str),
}

impl Stepping<'_> {
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

    fn claimed(&self) -> bool {
        fs::read_to_string(self.target.join(CLAIM))
            .is_ok_and(|claim| claim.trim_end_matches('\n') == self.worktree.display().to_string())
    }

    /// What this build's own prepare staged joins the target's record, which is a union: old
    /// artifacts stay when a tree moves on, so a revision built last week still counts. The
    /// lock is for two builds of one target finishing together.
    pub fn record(&self, token: &str) {
        let cache = Cache { dir: self.target };
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
                    .and_then(|m| dated(&to, &m));
                kept += u32::from(copied.is_ok());
            }
        }
        (kept > 0).then_some(kept)
    }

    /// Whether a crate a pin replaces still comes from where it came before, said when it does:
    /// most often the pinned tree's version does not meet the requirement the dependency states.
    pub fn unpinned(&self, names: &[String]) -> bool {
        let text = fs::read_to_string(self.worktree.join("Cargo.lock")).unwrap_or_default();
        let left: Vec<String> = (Lockfile { text: &text })
            .packages()
            .filter(|p| names.contains(&p.name) && !p.source.is_empty())
            .map(|p| format!("  {} from {}\n", p.name, p.source))
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
                let _ = now(&entry.path());
            }
            _ => {}
        }
    }
}

/// A `Cargo.lock`, read for each package's name and source.
struct Lockfile<'a> {
    text: &'a str,
}

/// One package of a lockfile.
struct Package {
    name: String,
    source: String,
}

impl<'a> Lockfile<'a> {
    /// Each `[[package]]`, its fields as their lines give them.
    fn packages(&self) -> impl Iterator<Item = Package> + 'a {
        self.text.split("[[package]]").skip(1).map(|block| {
            let field = |key: &str| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix(&format!("{key} = ")))
                    .map(|v| v.split_whitespace().next().unwrap_or("").replace('"', ""))
                    .unwrap_or_default()
            };
            Package {
                name: field("name"),
                source: field("source"),
            }
        })
    }
}
