use crate::{
    clock::Deadline,
    platform::{Host, Platform as _},
    tree::{
        builds::{Builds, Held},
        clocks::Clocks,
        copy::{Copier, empty, touch},
        git::{Commands, answer, said},
        packages::{Cache, Lines},
        runners::Runners,
        seed::{Seed, name},
        sweep::Sweep,
    },
};
use dibs_format::{
    Exit,
    wire::{GitDb, GitDbs, Nest, Prepare, Prepared, Revision, SOURCE_HASH, Seeded, Source},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

/// What a failed fetch says when the machine cannot see the remote at all, so the ref it was
/// asked for is very likely fine.
const CREDENTIALS: [&str; 5] = [
    "could not read Username",
    "Authentication failed",
    "terminal prompts disabled",
    "Permission denied (publickey)",
    "Repository not found",
];

/// Where dibs lays trees out on a machine, which it owns so that nothing is asked to follow a
/// convention by hand: a tree per commit or per sent checkout under `ws`, a build cache per repo
/// and comparison arm under `target`, and the machine's clones under `~/prog`.
pub struct Trees<'a> {
    pub scratch: &'a Path,
    pub home: &'a Path,
    pub cargo_home: &'a Path,
    pub keep_days: u64,
    pub target_keep_days: u64,
    pub seed_wait: Duration,
    pub copier: Copier,
    pub commands: Commands<'a>,
    pub say: &'a dyn Fn(&str),
}

/// A tree whose target was prepared this recently may be about to be entered by another call.
const IN_USE: Duration = Duration::from_secs(15 * 60);

/// What names this prepare's own temporary refs and directories: unique to it, as the shell's
/// `$$` was to its script.
#[derive(Debug, Clone)]
pub struct Stamp(String);

impl Stamp {
    fn next() -> Stamp {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Stamp(match n {
            0 => std::process::id().to_string(),
            n => format!("{}-{n}", std::process::id()),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A tree and its target, laid out.
struct Laid {
    worktree: PathBuf,
    target: PathBuf,
    sha: String,
    seeded: Option<Seeded>,
    reseeded: Option<u64>,
}

/// A local tree's target that had built clearly less of its lockfile than a sibling has, and
/// what replaced it.
struct Reseeded {
    seeded: Seeded,
    mine: u64,
}

impl Trees<'_> {
    pub fn prepare(&self, prepare: &Prepare) -> Result<Prepared, Exit> {
        if !plain_name(&prepare.repo) {
            (self.say)(&format!("dibs: {:?} is no repo's name\n", prepare.repo));
            return Err(Exit::Refused);
        }
        let packages = prepare
            .packages
            .as_ref()
            .map(|p| Lines::of(&p.lines))
            .filter(|lines| !lines.is_empty());
        let stamp = Stamp::next();
        let laid = match &prepare.source {
            Source::Fetched { reference, slot } => {
                self.fetched(prepare, reference, *slot, packages.as_ref(), &stamp)
            }
            Source::Local { key, .. } => self.local(prepare, key, packages.as_ref(), &stamp),
        };
        let laid = self.within_cap(laid)?;
        let sha = match &prepare.source {
            Source::Local { content, .. } => format!("local:{content}"),
            Source::Fetched { .. } => laid.sha.clone(),
        };
        let gitdbs = self.within_cap(Ok(self.gitdbs(&prepare.gitdbs)))?;
        Ok(Prepared {
            worktree: laid.worktree.display().to_string(),
            target: laid.target.display().to_string(),
            revision: Revision {
                repo: prepare.repo.clone(),
                sha,
            },
            seeded: laid.seeded,
            reseeded: laid.reseeded,
            gitdbs,
        })
    }

    /// A step's outcome, unless the job's cap passed meanwhile: a command stopped at the cap
    /// fails in whatever way the step reads it.
    fn within_cap<T>(&self, step: Result<T, Exit>) -> Result<T, Exit> {
        match self.overran() {
            true => Err(Exit::Overran),
            false => step,
        }
    }

    /// Whether the job's cap has passed, which ends the prepare with the job.
    fn overran(&self) -> bool {
        self.commands.deadline.passed()
    }

    /// Keyed by commit rather than by branch name: two agents on one branch at different
    /// commits get different trees instead of racing to check out over each other, and a rerun
    /// of one commit reuses its tree. The fetch goes into a ref of this prepare's own, never
    /// through the one FETCH_HEAD every prepare of the clone shares, which a second prepare
    /// fetching another branch would overwrite between this one's fetch and its read.
    fn fetched(
        &self,
        prepare: &Prepare,
        reference: &str,
        slot: u32,
        packages: Option<&Lines>,
        stamp: &Stamp,
    ) -> Result<Laid, Exit> {
        let repo = &prepare.repo;
        let source = self.home.join("prog").join(repo);
        if !source.join(".git").is_dir() {
            (self.say)(&format!("dibs: no clone at {}\n", source.display()));
            return Err(Exit::Setup);
        }
        let sha = self.resolve(&source, repo, reference, stamp)?;
        let short: String = sha.chars().take(12).collect();
        let worktree = self.nested(prepare).join(&short);
        self.made(worktree.parent().unwrap_or(self.scratch))?;
        self.add(&source, &worktree, &sha, repo)?;
        self.made_used(&worktree)?;
        self.nest(prepare, stamp)?;
        let mut suffix = prepare
            .nest
            .as_ref()
            .map(|n| format!("-{}", n.name))
            .unwrap_or_default();
        if slot > 0 {
            suffix += &format!("-arm{slot}");
        }
        let target = self.scratch.join("target").join(format!("{repo}{suffix}"));
        // The checkout's files may be older than the copied artifacts, so the copy's claim is
        // dropped and the first build dates them after it.
        let mut seeded = None;
        if !suffix.is_empty() && !target.is_dir() {
            seeded = self
                .seed(prepare, &target, &worktree, packages, 0, stamp)
                .run();
            let _ = fs::remove_file(target.join(".dibs-tree"));
        }
        self.cache(&target, prepare, packages)?;
        self.sweep(&worktree, &target);
        if let Ok(pruned) = self.commands.git(&source, &["worktree", "prune"]) {
            (self.say)(&String::from_utf8_lossy(&pruned.stderr));
        }
        Ok(Laid {
            worktree,
            target,
            sha: short,
            seeded,
            reseeded: None,
        })
    }

    /// The commit `reference` names, fetched first, or the reasons it names none said.
    fn resolve(
        &self,
        source: &Path,
        repo: &str,
        reference: &str,
        stamp: &Stamp,
    ) -> Result<String, Exit> {
        if reference.starts_with('-') {
            (self.say)(&format!("dibs: no such ref in {repo}: {reference}\n"));
            return Err(Exit::Setup);
        }
        let mine = format!("refs/dibs/prepare-{}", stamp.as_str());
        let git = |args: &[&str]| self.commands.git(source, args);
        let fetched = git(&["fetch", "-q", "origin", &format!("+{reference}:{mine}")]);
        let (sha, why) = match fetched.as_ref().is_ok_and(|o| o.status.success()) {
            true => {
                let sha = answer(git(&[
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("{mine}^{{commit}}"),
                ]));
                let _ = git(&["update-ref", "-d", &mine]);
                (sha, said(&fetched))
            }
            // A bare commit cannot be fetched by name from most servers, and a branch that
            // exists only on this machine cannot be fetched at all. Both resolve locally, by
            // their own name, which is not a slot anyone else can overwrite.
            false => {
                let all = git(&["fetch", "-q", "--all"]);
                let sha = answer(git(&[
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("{reference}^{{commit}}"),
                ]));
                (sha, format!("{}\n{}", said(&fetched), said(&all)))
            }
        };
        if let Some(sha) = sha {
            return Ok(sha);
        }
        if self.overran() {
            return Err(Exit::Overran);
        }
        let mut told = format!("dibs: no such ref in {repo}: {reference}\n");
        if CREDENTIALS.iter().any(|c| why.contains(c)) {
            told.push_str(&format!(
                "  The fetch failed on credentials, so nothing here can see that remote: a private\n  \
                 repo is the usual reason, and the ref itself is probably fine.\n  \
                 Send your working tree instead, which fetches nothing:  {repo}@local\n"
            ));
        }
        (self.say)(&told);
        Err(Exit::Setup)
    }

    /// The commit's worktree, detached so that it never moves under a job still measuring from
    /// it. Two prepares of one commit would both see none and both add it, so they take turns,
    /// per repo since the prune after touches every worktree of it.
    fn add(&self, source: &Path, worktree: &Path, sha: &str, repo: &str) -> Result<(), Exit> {
        let lock = self.held(
            &self.scratch.join("ws").join(repo).join(".prepare.lock"),
            self.commands.deadline,
            "could not lock the repo's trees",
        )?;
        if worktree.join(".git").exists() {
            drop(lock);
            return Ok(());
        }
        let add = |commands: &Commands| {
            let mut git = std::process::Command::new("git");
            git.arg("-C")
                .arg(source)
                .args(["worktree", "add", "--detach", "-q"])
                .arg(worktree)
                .arg(sha);
            commands.output(git)
        };
        let added = add(&self.commands).is_ok_and(|o| o.status.success());
        // A tree left behind by a crash is registered but absent; prune and try once more.
        let added = added || {
            if let Ok(pruned) = self.commands.git(source, &["worktree", "prune"]) {
                (self.say)(&String::from_utf8_lossy(&pruned.stderr));
            }
            let again = add(&self.commands);
            if let Ok(o) = &again {
                (self.say)(&String::from_utf8_lossy(&o.stderr));
            }
            again.is_ok_and(|o| o.status.success())
        };
        drop(lock);
        match added {
            true => Ok(()),
            false if self.overran() => Err(Exit::Overran),
            false => {
                (self.say)(&format!(
                    "dibs: could not add the worktree {}\n",
                    worktree.display()
                ));
                Err(Exit::Setup)
            }
        }
    }

    /// The same layout a fetched ref gets, without the fetch: the tree arrives by rsync, into a
    /// target of its own, so two local trees of one repo cannot hand each other a binary.
    fn local(
        &self,
        prepare: &Prepare,
        key: &str,
        packages: Option<&Lines>,
        stamp: &Stamp,
    ) -> Result<Laid, Exit> {
        let repo = &prepare.repo;
        let worktree = self.nested(prepare).join(format!("local-{key}"));
        let nest = prepare
            .nest
            .as_ref()
            .map(|n| format!("-{}", n.name))
            .unwrap_or_default();
        let target = self
            .scratch
            .join("target")
            .join(format!("{repo}-local-{key}{nest}"));
        self.made(worktree.parent().unwrap_or(self.scratch))?;
        let turn = self.turn(&worktree)?;
        let (mut seeded, mut reseeded) = (None, None);
        if !worktree.is_dir() && !target.is_dir() {
            seeded = self
                .seed(prepare, &target, &worktree, packages, 0, stamp)
                .run();
        } else if let Some(packages) = packages
            && worktree.is_dir()
            && target.is_dir()
            && let Some(again) = self.reseed(prepare, &target, &worktree, packages, stamp)
        {
            seeded = Some(again.seeded);
            reseeded = Some(again.mine);
        }
        self.made(&worktree)?;
        self.made_used(&worktree)?;
        self.cache(&target, prepare, packages)?;
        drop(turn);
        self.nest(prepare, stamp)?;
        self.sweep(&worktree, &target);
        Ok(Laid {
            worktree,
            target,
            sha: String::new(),
            seeded,
            reseeded,
        })
    }

    /// An existing tree whose target has built clearly less of its lockfile than a sibling has,
    /// as every tree has once a dependency moves, starts again from that sibling's target and
    /// sources as a new tree would, and the sync after it rewrites what differs. The sibling is
    /// copied before anything of the tree moves, and the tree is replaced only by renames. Never
    /// while a build holds its target, nor one prepared minutes ago, which another call's job may
    /// be about to enter, nor one a process works in, asked again once the copy has landed.
    fn reseed(
        &self,
        prepare: &Prepare,
        target: &Path,
        worktree: &Path,
        packages: &Lines,
        stamp: &Stamp,
    ) -> Option<Reseeded> {
        let cache = Cache { dir: target };
        let in_use = || cache.used_within(IN_USE) || worked_in(&[worktree, target]);
        if in_use() {
            return None;
        }
        let mine = cache.record().map_or(0, |own| packages.shared_with(&own));
        let floor = mine + packages.len().div_ceil(10);
        let held = (Builds { target }).all_exclusive()?;
        let seed = Seed {
            replacing: true,
            ..self.seed(prepare, target, worktree, Some(packages), floor, stamp)
        };
        let copied = seed.copy()?;
        let seeded = match in_use() {
            true => {
                copied.discard();
                None
            }
            false => copied.replace(target, worktree, stamp, self.say),
        };
        drop(held);
        seeded.map(|seeded| Reseeded { seeded, mine })
    }

    /// A sent tree to this prepare alone, from deciding what it starts from until its target is
    /// marked used, so a reseed that copied for minutes never replaces a tree handed over since.
    fn turn(&self, worktree: &Path) -> Result<Held, Exit> {
        let path = worktree.with_file_name(format!(".{}.lock", name(worktree)));
        if let Ok(Some(held)) = Held::exclusive_by(&path, Deadline::after(Some(Duration::ZERO))) {
            return Ok(held);
        }
        (self.say)(&format!(
            "dibs: waiting for another prepare of {}\n",
            worktree.display()
        ));
        self.held(&path, self.commands.deadline, "could not lock the tree")
    }

    /// `lock` taken before `deadline`, or the prepare overran.
    fn held(&self, lock: &Path, deadline: Deadline, why: &str) -> Result<Held, Exit> {
        Held::exclusive_by(lock, deadline)
            .map_err(|e| self.failed(why, &e))?
            .ok_or(Exit::Overran)
    }

    fn seed<'s>(
        &'s self,
        prepare: &'s Prepare,
        target: &'s Path,
        worktree: &'s Path,
        packages: Option<&'s Lines>,
        floor: u64,
        stamp: &'s Stamp,
    ) -> Seed<'s> {
        Seed {
            scratch: self.scratch,
            repo: &prepare.repo,
            target,
            worktree,
            packages,
            floor,
            fresh: &prepare.fresh,
            wait: self.commands.deadline.within(self.seed_wait),
            copier: self.copier,
            stamp,
            replacing: false,
            say: self.say,
        }
    }

    /// Where the repo's trees go: under the pinned trees' nest when there is one.
    fn nested(&self, prepare: &Prepare) -> PathBuf {
        let repo = self.scratch.join("ws").join(&prepare.repo);
        match &prepare.nest {
            Some(nest) => repo.join(&nest.name),
            None => repo,
        }
    }

    /// The nest's config, beside the tree's own where cargo reads it after it, so the tree stays
    /// what was sent or checked out. Marked used with its tree, since a sweep judges the
    /// directory it sits in by that.
    fn nest(&self, prepare: &Prepare, stamp: &Stamp) -> Result<(), Exit> {
        let Some(Nest { name, config }) = &prepare.nest else {
            return Ok(());
        };
        let nest = self.scratch.join("ws").join(&prepare.repo).join(name);
        let cargo = nest.join(".cargo");
        let written = cargo.join(format!("config.toml.{}", stamp.as_str()));
        touch(&nest.join(".dibs-used"))
            .and_then(|()| fs::create_dir_all(&cargo))
            .and_then(|()| fs::write(&written, config))
            .and_then(|()| fs::rename(&written, cargo.join("config.toml")))
            .map_err(|e| self.failed("could not write the pins' cargo config", &e))
    }

    /// The target, marked used, with this prepare's lockfile staged beside it until a build of it
    /// succeeds: a target is never credited with a lockfile whose build failed or never ran.
    fn cache(
        &self,
        target: &Path,
        prepare: &Prepare,
        packages: Option<&Lines>,
    ) -> Result<(), Exit> {
        self.made(target)?;
        self.made(&self.scratch.join("out"))?;
        empty(&target.join(".dibs-used"))
            .map_err(|e| self.failed("could not mark the target", &e))?;
        let cache = Cache { dir: target };
        if let (Some(lines), Some(staged)) = (packages, &prepare.packages) {
            cache
                .stage(&staged.token, lines)
                .map_err(|e| self.failed("could not stage the lockfile", &e))?;
        }
        cache.forget_unbuilt();
        Ok(())
    }

    fn sweep(&self, worktree: &Path, target: &Path) {
        Sweep {
            scratch: self.scratch,
            clocks: Clocks {
                keep_days: self.keep_days,
                target_keep_days: self.target_keep_days,
            },
            worktree,
            target,
            runners: &Runners {
                dir: self.home.join(".cache/dibs/runner"),
                own: SOURCE_HASH.map(str::to_string),
            },
            commands: &self.commands,
            say: self.say,
        }
        .run();
    }

    /// Which of the commits asked about the machine's cargo lacks.
    fn gitdbs(&self, asked: &[GitDb]) -> Option<GitDbs> {
        if asked.is_empty() {
            return None;
        }
        let dir = self.cargo_home.join("git/db");
        let missing = asked
            .iter()
            .filter(|db| {
                let has = self.commands.git(
                    &dir.join(&db.name),
                    &["cat-file", "-e", &format!("{}^{{commit}}", db.commit)],
                );
                !has.is_ok_and(|o| o.status.success())
            })
            .cloned()
            .collect();
        Some(GitDbs {
            dir: dir.display().to_string(),
            missing,
        })
    }

    fn made(&self, dir: &Path) -> Result<(), Exit> {
        fs::create_dir_all(dir)
            .map_err(|e| self.failed(&format!("could not make {}", dir.display()), &e))
    }

    fn made_used(&self, worktree: &Path) -> Result<(), Exit> {
        touch(&worktree.join(".dibs-used")).map_err(|e| self.failed("could not mark the tree", &e))
    }

    fn failed(&self, what: &str, error: &io::Error) -> Exit {
        (self.say)(&format!("dibs: {what}: {error}\n"));
        match error.kind() {
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => Exit::NoRoom,
            _ => Exit::Setup,
        }
    }
}

/// Whether a process works in one of `dirs`, or below it.
fn worked_in(dirs: &[&Path]) -> bool {
    Host::processes()
        .iter()
        .any(|p| Host::cwd(p.pid).is_some_and(|cwd| dirs.iter().any(|dir| cwd.starts_with(dir))))
}

/// A name that is one path component, which a repo's directory and its trees are named by.
fn plain_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains('/')
}
