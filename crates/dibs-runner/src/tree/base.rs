use crate::{
    clock::Deadline,
    platform::{Host, Platform as _},
    tree::{
        builds::{Builds, FileLock},
        clocks::{Clocks, Removal, USED},
        copy::{Copier, Reflinks, empty, now, remove_all, touch},
        error::{Named, PrepareError},
        git::{Commands, Git, answer, said},
        packages::{Cache, Lines},
        seed::{Seed, Seedling},
        sweep::{PREPARE_LOCK, Sweep},
    },
};
use dibs_format::wire::{GitDb, GitDbs, Nest, Prepare, Prepared, Revision, Seeded, Source};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

/// Where dibs lays trees out on a machine, which it owns so that nothing is asked to follow a
/// convention by hand: a tree per commit or per sent checkout under `ws`, a build cache per repo
/// and comparison arm under `target`, and the machine's clones under `~/prog`.
pub struct Trees<'a> {
    config: TreeConfig<'a>,
    copier: Copier,
    commands: Commands<'a>,
    say: &'a dyn Fn(&str),
}

/// Where a machine's trees live, and how it keeps and seeds them.
#[derive(Clone, Copy)]
pub struct TreeConfig<'a> {
    pub scratch: &'a Path,
    pub home: &'a Path,
    pub cargo_home: &'a Path,
    pub clocks: Clocks,
    /// How long a new tree waits for a sibling's build that will leave it more of its lockfile.
    pub seed_wait: Duration,
    pub reflinks: Reflinks,
}

/// A tree whose target was prepared this recently may be about to be entered by another call.
const IN_USE: Duration = Duration::from_secs(15 * 60);
/// A reseed is worth its copy only from a sibling sharing more of the lockfile than the tree's
/// own build by at least one package in this many.
const RESEED_GAIN: u64 = 10;

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

impl<'a> Trees<'a> {
    pub fn new(config: TreeConfig<'a>, commands: Commands<'a>, say: &'a dyn Fn(&str)) -> Self {
        Trees {
            config,
            copier: Copier::new(config.reflinks),
            commands,
            say,
        }
    }

    pub fn prepare(&self, prepare: &Prepare) -> Result<Prepared, PrepareError> {
        Trees::placed(prepare)?;
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

    /// Every value of the request that names a path, refused unless it names one inside the
    /// place it is for: a repo's name and a key that climbed out of `ws` would lay a tree out
    /// anywhere, and a fresh path that did would remove anything.
    fn placed(prepare: &Prepare) -> Result<(), PrepareError> {
        Named::Repo.check(&prepare.repo)?;
        if let Source::Local { key, .. } = &prepare.source {
            Named::Key.check(key)?;
        }
        if let Some(nest) = &prepare.nest {
            Named::Nest.check(&nest.name)?;
        }
        if let Some(packages) = &prepare.packages {
            Named::Token.check(&packages.token)?;
        }
        for fresh in &prepare.fresh {
            Named::Fresh.check(fresh)?;
        }
        for db in &prepare.gitdbs {
            Named::GitDb.check(&db.name)?;
        }
        Ok(())
    }

    /// A step's outcome, unless the job's cap passed meanwhile: a command stopped at the cap
    /// fails in whatever way the step reads it.
    fn within_cap<T>(&self, step: Result<T, PrepareError>) -> Result<T, PrepareError> {
        match self.overran() {
            true => Err(PrepareError::Overran),
            false => step,
        }
    }

    /// Whether the job's cap has passed, which ends the prepare with the job.
    fn overran(&self) -> bool {
        self.commands.deadline().passed()
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
    ) -> Result<Laid, PrepareError> {
        let repo = &prepare.repo;
        let source = self.config.home.join("prog").join(repo);
        if !source.join(".git").is_dir() {
            return Err(PrepareError::NoClone(source));
        }
        let sha = self.resolve(&source, repo, reference, stamp)?;
        let short: String = sha.chars().take(12).collect();
        let worktree = self.nested(prepare).join(&short);
        let mut suffix = prepare
            .nest
            .as_ref()
            .map(|n| format!("-{}", n.name))
            .unwrap_or_default();
        if slot > 0 {
            suffix += &format!("-arm{slot}");
        }
        let target = self
            .config
            .scratch
            .join("target")
            .join(format!("{repo}{suffix}"));
        self.made(worktree.parent().unwrap_or(self.config.scratch))?;
        let trees = self.repo_turn(repo)?;
        self.add(&source, &worktree, &sha)?;
        self.made_used(&worktree)?;
        self.revive(prepare, &worktree);
        drop(trees);
        self.nest(prepare, stamp)?;
        let target_turn = self.target_turn(&target)?;
        // The checkout's files may be older than the copied artifacts, so the copy's claim is
        // dropped and the first build dates them after it.
        let mut seeded = None;
        if !suffix.is_empty() && !target.is_dir() {
            seeded = self
                .seed(prepare, &target, &worktree, packages, 0, stamp)
                .run(self.commands.unstopped());
            let _ = fs::remove_file(target.join(".dibs-tree"));
        }
        self.cache(&target, prepare, packages)?;
        drop(target_turn);
        self.sweep();
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
    ) -> Result<String, PrepareError> {
        let no_ref = |said: String| PrepareError::NoRef {
            repo: repo.to_string(),
            reference: reference.to_string(),
            said,
        };
        if reference.starts_with('-') {
            return Err(no_ref(String::new()));
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
        match self.overran() {
            true => Err(PrepareError::Overran),
            false => Err(no_ref(why)),
        }
    }

    /// The repo's trees to this prepare alone: two prepares of one commit would both see no tree
    /// and both add it, the prune after touches every worktree of the repo, and a sweep removes
    /// none of them meanwhile.
    fn repo_turn(&self, repo: &str) -> Result<FileLock, PrepareError> {
        self.held(
            &self.config.scratch.join("ws").join(repo).join(PREPARE_LOCK),
            self.commands.deadline(),
            "could not lock the repo's trees",
        )
    }

    /// The commit's worktree, detached so that it never moves under a job still measuring from
    /// it. Holding the repo's turn.
    fn add(&self, source: &Path, worktree: &Path, sha: &str) -> Result<(), PrepareError> {
        if worktree.join(".git").exists() {
            if !self.half_checked_out(worktree) {
                return Ok(());
            }
            (self.say)(&format!(
                "dibs: {} was left half checked out, so it is checked out again\n",
                worktree.display()
            ));
            let mut remove = Git(source).command();
            remove
                .args(["worktree", "remove", "--force", "--force"])
                .arg(worktree);
            let _ = self.commands.output(remove);
            remove_all(worktree);
        }
        let add = |commands: &Commands| {
            let mut git = Git(source).command();
            git.args(["worktree", "add", "--detach", "-q"])
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
        match added {
            true => Ok(()),
            false if self.overran() => Err(PrepareError::Overran),
            false => Err(PrepareError::NotAdded(worktree.to_path_buf())),
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
    ) -> Result<Laid, PrepareError> {
        let repo = &prepare.repo;
        let worktree = self.nested(prepare).join(format!("local-{key}"));
        let nest = prepare
            .nest
            .as_ref()
            .map(|n| format!("-{}", n.name))
            .unwrap_or_default();
        let target = self
            .config
            .scratch
            .join("target")
            .join(format!("{repo}-local-{key}{nest}"));
        self.made(worktree.parent().unwrap_or(self.config.scratch))?;
        let trees = self.repo_turn(repo)?;
        self.revive(prepare, &worktree);
        drop(trees);
        let turn = self.turn(&worktree)?;
        let target_turn = self.target_turn(&target)?;
        let (mut seeded, mut reseeded) = (None, None);
        if !worktree.is_dir() && !target.is_dir() {
            seeded = self
                .seed(prepare, &target, &worktree, packages, 0, stamp)
                .run(self.commands.unstopped());
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
        drop(target_turn);
        drop(turn);
        self.nest(prepare, stamp)?;
        self.sweep();
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
        let cache = Cache::new(target);
        let in_use = || cache.used_within(IN_USE) || worked_in(&[worktree, target]);
        if in_use() {
            return None;
        }
        let mine = cache.record().map_or(0, |own| packages.shared_with(&own));
        let floor = mine + packages.len().div_ceil(RESEED_GAIN);
        let held = Builds::new(target).all_exclusive()?;
        let seed = self.seed(prepare, target, worktree, Some(packages), floor, stamp);
        let copied = seed.replacement()?;
        let seeded = match in_use() {
            true => {
                copied.discard();
                None
            }
            false => copied.replace(target, worktree, stamp, self.say, self.commands.unstopped()),
        };
        drop(held);
        seeded.map(|seeded| Reseeded { seeded, mine })
    }

    /// A tree a stopped `git worktree add` left: git writes the index once every file is out, so
    /// one without it was cut short. Never one a process works in.
    fn half_checked_out(&self, worktree: &Path) -> bool {
        let index = answer(
            self.commands
                .git(worktree, &["rev-parse", "--git-path", "index"]),
        );
        index.is_some_and(|index| !worktree.join(index).exists()) && !worked_in(&[worktree])
    }

    /// The tree this prepare may reuse, and the nest it sits in, marked used holding the repo's
    /// turn, under which a sweep judges them again before it removes either.
    fn revive(&self, prepare: &Prepare, worktree: &Path) {
        let _ = now(&worktree.join(USED));
        if prepare.nest.is_some() {
            let _ = now(&self.nested(prepare).join(USED));
        }
    }

    /// The target to this prepare alone, from deciding what it starts from until it is marked
    /// used: a sweep removes no target a prepare holds, and its marker's time, which says
    /// whether another call may be about to enter it, is left until then.
    fn target_turn(&self, target: &Path) -> Result<FileLock, PrepareError> {
        self.made(target.parent().unwrap_or(self.config.scratch))?;
        self.held(
            &FileLock::beside(target),
            self.commands.deadline(),
            "could not lock the target",
        )
    }

    /// A sent tree to this prepare alone, from deciding what it starts from until its target is
    /// marked used, so a reseed that copied for minutes never replaces a tree handed over since.
    fn turn(&self, worktree: &Path) -> Result<FileLock, PrepareError> {
        let path = FileLock::beside(worktree);
        if let Ok(Some(held)) = FileLock::exclusive_by(&path, Deadline::after(Some(Duration::ZERO)))
        {
            return Ok(held);
        }
        (self.say)(&format!(
            "dibs: waiting for another prepare of {}\n",
            worktree.display()
        ));
        self.held(&path, self.commands.deadline(), "could not lock the tree")
    }

    /// `lock` taken before `deadline`, or the prepare overran.
    fn held(&self, lock: &Path, deadline: Deadline, why: &str) -> Result<FileLock, PrepareError> {
        FileLock::exclusive_by(lock, deadline)
            .map_err(|e| PrepareError::io(why, e))?
            .ok_or(PrepareError::Overran)
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
        Seed::new(
            Seedling {
                scratch: self.config.scratch,
                repo: &prepare.repo,
                target,
                worktree,
                packages,
                fresh: &prepare.fresh,
                stamp,
            },
            floor,
            self.commands.deadline().within(self.config.seed_wait),
            self.copier,
            self.say,
        )
    }

    /// Where the repo's trees go: under the pinned trees' nest when there is one.
    fn nested(&self, prepare: &Prepare) -> PathBuf {
        let repo = self.config.scratch.join("ws").join(&prepare.repo);
        match &prepare.nest {
            Some(nest) => repo.join(&nest.name),
            None => repo,
        }
    }

    /// The nest's config, beside the tree's own where cargo reads it after it, so the tree stays
    /// what was sent or checked out. Marked used with its tree, since a sweep judges the
    /// directory it sits in by that.
    fn nest(&self, prepare: &Prepare, stamp: &Stamp) -> Result<(), PrepareError> {
        let Some(Nest { name, config }) = &prepare.nest else {
            return Ok(());
        };
        let nest = self
            .config
            .scratch
            .join("ws")
            .join(&prepare.repo)
            .join(name);
        let cargo = nest.join(".cargo");
        let written = cargo.join(format!("config.toml.{}", stamp.as_str()));
        touch(&nest.join(USED))
            .and_then(|()| fs::create_dir_all(&cargo))
            .and_then(|()| fs::write(&written, config))
            .and_then(|()| fs::rename(&written, cargo.join("config.toml")))
            .map_err(|e| PrepareError::io("could not write the pins' cargo config", e))
    }

    /// The target, marked used, with this prepare's lockfile staged beside it until a build of it
    /// succeeds: a target is never credited with a lockfile whose build failed or never ran.
    fn cache(
        &self,
        target: &Path,
        prepare: &Prepare,
        packages: Option<&Lines>,
    ) -> Result<(), PrepareError> {
        self.made(target)?;
        self.made(&self.config.scratch.join("out"))?;
        empty(&target.join(USED)).map_err(|e| PrepareError::io("could not mark the target", e))?;
        let cache = Cache::new(target);
        if let (Some(lines), Some(staged)) = (packages, &prepare.packages) {
            cache
                .stage(&staged.token, lines)
                .map_err(|e| PrepareError::io("could not stage the lockfile", e))?;
        }
        cache.forget_unbuilt();
        Ok(())
    }

    fn sweep(&self) {
        let removal = Removal::new(Some(&self.commands), self.say);
        Sweep::new(
            self.config.scratch,
            self.config.home,
            self.config.clocks,
            removal,
        )
        .run();
    }

    /// Which of the commits asked about the machine's cargo lacks.
    fn gitdbs(&self, asked: &[GitDb]) -> Option<GitDbs> {
        if asked.is_empty() {
            return None;
        }
        let dir = self.config.cargo_home.join("git/db");
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

    fn made(&self, dir: &Path) -> Result<(), PrepareError> {
        fs::create_dir_all(dir)
            .map_err(|e| PrepareError::io(&format!("could not make {}", dir.display()), e))
    }

    fn made_used(&self, worktree: &Path) -> Result<(), PrepareError> {
        touch(&worktree.join(USED)).map_err(|e| PrepareError::io("could not mark the tree", e))
    }
}

/// Whether a process works in one of `dirs`, or below it.
fn worked_in(dirs: &[&Path]) -> bool {
    Host::processes()
        .iter()
        .any(|p| Host::cwd(p.pid).is_some_and(|cwd| dirs.iter().any(|dir| cwd.starts_with(dir))))
}
