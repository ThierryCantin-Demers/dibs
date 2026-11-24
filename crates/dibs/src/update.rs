//! `dibs --update`, and the two notices: a session is told once when dibs changed under it, and
//! once when its upstream has a newer dibs or newer recipes than this computer runs.

use crate::{caller::Caller, git::Git, paths::Paths};
use dibs_format::{Exit, Span};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

/// A stamp untouched this long belongs to a session that is gone.
const STAMP_LIFETIME: Duration = Duration::from_secs(31 * Span::DAY.0);
const LISTED: usize = 10;
/// How long an upstream's answer stands before a call asks again.
const ASKED_EVERY: Duration = Duration::from_secs(3600);
/// How soon a call asks again when the last ask did not finish, as when its call was short.
const ASKED_AGAIN_AFTER: Duration = Duration::from_secs(300);
/// `0` turns the upstream check off, for a computer that should never reach the network for it.
pub const UPDATE_CHECK: &str = "DIBS_UPDATE_CHECK";

/// Both notices, told to a session at most once each.
pub fn tell_once(caller: &Caller) {
    ChangeNotice::tell_once(caller);
    Upstream::tell_once(caller);
}

/// What this binary was built from, as its build script stamped it.
pub struct Build;

impl Build {
    /// The commit, when it was built from a clone.
    pub const COMMIT: Option<&str> = option_env!("DIBS_COMMIT");

    /// The clone: what `--update` pulls, and where the change notice reads what arrived.
    fn clone_dir() -> &'static Path {
        Path::new(env!("DIBS_CLONE"))
    }
}

fn git(clone: &Path, args: &[&str]) -> Option<String> {
    let text = Git(clone).run(args).ok()?.trim_end().to_string();
    (!text.is_empty()).then_some(text)
}

/// A pull, which says on stderr why it failed.
fn pulled(clone: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(["pull", "--ff-only", "--quiet"])
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether a git command in the clone succeeds, its output left unread.
fn git_ok(clone: &Path, args: &[&str]) -> bool {
    Git(clone).run(args).is_ok()
}

/// `dibs --update`: the clone fast-forwarded and what arrived listed, a reinstall when anything
/// arrived or the installed build is from another commit, then the recipes pulled.
pub struct Update {
    pub clone: PathBuf,
    /// The commit install.sh stamped into the running build.
    pub installed: Option<String>,
    pub recipes: Option<PathBuf>,
    pub notice: Option<ChangeNotice>,
    pub caller: Caller,
    /// Where the running build is installed, so install.sh replaces it rather than one
    /// elsewhere on PATH: a trial install updates the trial.
    pub prefix: Option<PathBuf>,
}

impl Update {
    pub fn of_this_build() -> Update {
        let paths = Paths::from_env();
        Update {
            clone: Build::clone_dir().to_path_buf(),
            installed: Build::COMMIT.map(str::to_string),
            recipes: paths.recipes(),
            notice: paths.seen().map(ChangeNotice::of_this_build),
            caller: Caller::from_env(),
            prefix: Update::installed_under(),
        }
    }

    /// The directory above the `bin` this binary runs from, when it runs from one.
    fn installed_under() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
        let bin = exe.parent()?;
        (bin.file_name()? == "bin").then(|| bin.parent().map(Path::to_path_buf))?
    }

    pub fn run(&self) -> i32 {
        self.run_into(&mut io::stdout(), &mut io::stderr())
    }

    pub fn run_into(&self, out: &mut impl Write, err: &mut impl Write) -> i32 {
        self.update(out, err)
            .unwrap_or_else(|exit| i32::from(exit.code()))
    }

    fn update(&self, out: &mut impl Write, err: &mut impl Write) -> Result<i32, Exit> {
        let clone = &self.clone;
        if !git_ok(clone, &["rev-parse", "--git-dir"]) {
            let _ = writeln!(
                err,
                "dibs: {} is not inside a git clone, so there is nothing to pull.",
                clone.display()
            );
            let _ = writeln!(err, "  Install from a clone:  ./install.sh");
            return Err(Exit::Refused);
        }
        let before = git(clone, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
        if !pulled(clone) {
            let _ = writeln!(
                err,
                "dibs: could not fast-forward {}, so nothing was updated.",
                clone.display()
            );
            return Err(Exit::Failed);
        }
        let after = git(clone, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
        Pulled {
            what: "dibs",
            clone,
            before: &before,
            after: &after,
        }
        .tell(out);
        if before != after || self.installed.as_deref() != Some(after.as_str()) {
            let mut install = Command::new("bash");
            install.arg(clone.join("install.sh"));
            if let Some(prefix) = self
                .prefix
                .as_ref()
                .filter(|_| std::env::var_os("PREFIX").is_none())
            {
                install.env("PREFIX", prefix);
            }
            let installed = install.status().is_ok_and(|s| s.success());
            if !installed {
                let _ = writeln!(err, "dibs: install.sh failed; the clone is at {after}");
                return Err(Exit::Failed);
            }
        }
        self.pull_recipes(out, err)?;
        if let Some(notice) = &self.notice {
            notice.record(&self.caller, &after);
        }
        Ok(0)
    }

    fn pull_recipes(&self, out: &mut impl Write, err: &mut impl Write) -> Result<(), Exit> {
        let Some(recipes) = &self.recipes else {
            return Ok(());
        };
        if !git_ok(recipes, &["rev-parse", "--abbrev-ref", "@{u}"]) {
            if recipes.is_dir() {
                let _ = writeln!(
                    out,
                    "recipes in {} are not a clone with an upstream, so they were left alone",
                    recipes.display()
                );
            }
            return Ok(());
        }
        let before = git(recipes, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
        if !pulled(recipes) {
            let _ = writeln!(
                err,
                "dibs: could not fast-forward the recipes in {}",
                recipes.display()
            );
            return Err(Exit::Failed);
        }
        let after = git(recipes, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
        Pulled {
            what: "recipes",
            clone: recipes,
            before: &before,
            after: &after,
        }
        .tell(out);
        Ok(())
    }
}

/// One clone's pull, as the update reports it.
struct Pulled<'a> {
    what: &'a str,
    clone: &'a Path,
    before: &'a str,
    after: &'a str,
}

impl Pulled<'_> {
    fn tell(&self, out: &mut impl Write) {
        let Pulled {
            what,
            before,
            after,
            ..
        } = self;
        if before == after {
            let _ = writeln!(out, "{what} {after}, already current");
            return;
        }
        let _ = writeln!(out, "{what} {before} -> {after}");
        let range = format!("{before}..{after}");
        let log =
            git(self.clone, &["log", "--oneline", "--no-decorate", &range]).unwrap_or_default();
        for line in log.lines() {
            let _ = writeln!(out, "  {line}");
        }
    }
}

/// Where each session's last seen version is kept, and the clone whose log says what changed.
pub struct ChangeNotice {
    pub seen: PathBuf,
    pub clone: PathBuf,
}

impl ChangeNotice {
    /// Told to a session once, where this computer keeps what each session has seen.
    pub fn tell_once(caller: &Caller) {
        if let Some(seen) = Paths::from_env().seen() {
            ChangeNotice::of_this_build(seen).tell(caller);
        }
    }

    pub fn of_this_build(seen: PathBuf) -> ChangeNotice {
        ChangeNotice {
            seen,
            clone: Build::clone_dir().to_path_buf(),
        }
    }

    /// Records this build's commit as the session's version, and says on stderr what changed
    /// since its last call.
    pub fn tell(&self, caller: &Caller) {
        if let Some(now) = Build::COMMIT
            && let Some(text) = self.record(caller, now)
        {
            eprint!("{text}");
        }
    }

    /// Records `now` as the session's version, and gives what changed since its last call.
    pub fn record(&self, caller: &Caller, now: &str) -> Option<String> {
        let stamp = self.seen.join(caller.file_name());
        let was = std::fs::read_to_string(&stamp).unwrap_or_default();
        let was = was.trim_end();
        if was == now {
            return None;
        }
        if std::fs::create_dir_all(&self.seen).is_ok() {
            let _ = std::fs::write(&stamp, format!("{now}\n"));
        }
        self.forget_old_sessions();
        (!was.is_empty()).then(|| self.text(was, now))
    }

    fn forget_old_sessions(&self) {
        let Ok(entries) = std::fs::read_dir(&self.seen) else {
            return;
        };
        let old = |meta: &std::fs::Metadata| {
            meta.modified()
                .ok()
                .and_then(|m| SystemTime::now().duration_since(m).ok())
                .is_some_and(|age| age >= STAMP_LIFETIME)
        };
        for entry in entries.flatten() {
            if entry.metadata().is_ok_and(|m| m.is_file() && old(&m)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn text(&self, was: &str, now: &str) -> String {
        let clone = &self.clone;
        let mut text = format!("dibs changed since this session last ran it: {was} -> {now}\n");
        let range = format!("{was}..{now}");
        if git_ok(clone, &["merge-base", "--is-ancestor", was, now]) {
            let count: usize = git(clone, &["rev-list", "--count", &range])
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let listed = format!("-{LISTED}");
            let log = git(
                clone,
                &["log", "--oneline", "--no-decorate", &listed, &range],
            )
            .unwrap_or_default();
            for line in log.lines() {
                text.push_str(&format!("  {line}\n"));
            }
            if count > LISTED {
                text.push_str(&format!(
                    "  and {} more:  git -C {} log {range}\n",
                    count - LISTED,
                    clone.display()
                ));
            }
        }
        text.push_str("  Flags and output you remember may be wrong now. Read dibs --help, and\n");
        text.push_str(&format!(
            "  {}/dibs-agent-rules.md for how it is meant to be used.\n",
            clone.display()
        ));
        text
    }
}

/// What the upstreams of the clone and of the recipes say their heads are, against what runs
/// here. It is asked at most once an hour, beside a call and never waited for, so a slow or absent
/// network costs a call nothing and the notice comes a call later.
pub struct Upstream {
    /// Where the last answer is kept, with when it was asked.
    pub cache: PathBuf,
    /// Where each session's last notice is kept, beside the change notice's stamps.
    pub seen: PathBuf,
    pub clone: PathBuf,
    /// The commit the running build was made from.
    pub installed: Option<String>,
    pub recipes: Option<PathBuf>,
}

/// What the upstreams said, as kept in the cache.
#[derive(Default, PartialEq, Debug)]
struct Heads {
    asked: u64,
    dibs: Option<String>,
    recipes: Option<String>,
}

impl Heads {
    fn read(path: &Path) -> Heads {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut heads = Heads::default();
        for line in text.lines() {
            match line.split_once(' ') {
                Some(("asked", at)) => heads.asked = at.parse().unwrap_or(0),
                Some(("dibs", head)) => heads.dibs = Some(head.to_string()),
                Some(("recipes", head)) => heads.recipes = Some(head.to_string()),
                _ => {}
            }
        }
        heads
    }

    /// Written whole and renamed into place, so a call that ends mid-write leaves the old answer.
    fn write(&self, path: &Path) {
        let mut text = format!("asked {}\n", self.asked);
        for (name, head) in [("dibs", &self.dibs), ("recipes", &self.recipes)] {
            if let Some(head) = head {
                text.push_str(&format!("{name} {head}\n"));
            }
        }
        let Some(dir) = path.parent() else { return };
        let partial = dir.join(format!(".upstream.{}", std::process::id()));
        if std::fs::create_dir_all(dir).is_ok() && std::fs::write(&partial, text).is_ok() {
            let _ = std::fs::rename(&partial, path);
        }
    }
}

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Upstream {
    /// Tells a session once about a newer upstream, from the last answer, and asks again
    /// beside the call when that answer is old.
    pub fn tell_once(caller: &Caller) {
        if std::env::var(UPDATE_CHECK).is_ok_and(|v| v == "0") {
            return;
        }
        let paths = Paths::from_env();
        let (Some(cache), Some(seen)) = (paths.upstream(), paths.seen()) else {
            return;
        };
        let upstream = Upstream {
            cache,
            seen,
            clone: Build::clone_dir().to_path_buf(),
            installed: Build::COMMIT.map(str::to_string),
            recipes: paths.recipes(),
        };
        if let Some(text) = upstream.notice(caller) {
            eprint!("{text}");
        }
        if upstream.due(epoch_now()) {
            std::thread::spawn(move || upstream.ask(epoch_now()));
        }
    }

    /// Whether the last answer is old enough to ask again. Marks the ask as begun, so the calls
    /// of the next few minutes leave it to this one.
    fn due(&self, now: u64) -> bool {
        let mut heads = Heads::read(&self.cache);
        if now.saturating_sub(heads.asked) < ASKED_EVERY.as_secs() {
            return false;
        }
        heads.asked = now.saturating_sub(ASKED_EVERY.as_secs() - ASKED_AGAIN_AFTER.as_secs());
        heads.write(&self.cache);
        true
    }

    /// Asks both upstreams for their heads and keeps the answer.
    pub fn ask(&self, now: u64) {
        let heads = Heads {
            asked: now,
            dibs: head_upstream(&self.clone),
            recipes: self.recipes.as_deref().and_then(head_upstream),
        };
        heads.write(&self.cache);
    }

    /// What to tell `caller`, the first time the last answer names something newer.
    pub fn notice(&self, caller: &Caller) -> Option<String> {
        let heads = Heads::read(&self.cache);
        let dibs = heads.dibs.filter(|head| {
            self.installed
                .as_deref()
                .is_some_and(|at| newer(&self.clone, head, at))
        });
        let recipes = heads.recipes.filter(|head| {
            self.recipes
                .as_deref()
                .is_some_and(|dir| newer(dir, head, "HEAD"))
        });
        if dibs.is_none() && recipes.is_none() {
            return None;
        }
        let told = format!(
            "{} {}\n",
            dibs.as_deref().unwrap_or("-"),
            recipes.as_deref().unwrap_or("-")
        );
        let stamp = self.seen.join(format!("{}.upstream", caller.file_name()));
        if std::fs::read_to_string(&stamp).is_ok_and(|was| was == told) {
            return None;
        }
        if std::fs::create_dir_all(&self.seen).is_ok() {
            let _ = std::fs::write(&stamp, &told);
        }
        let what = match (&dibs, &recipes) {
            (Some(_), Some(_)) => "a newer dibs and newer recipes are",
            (Some(_), None) => "a newer dibs is",
            _ => "newer recipes are",
        };
        let short = |head: &str| head.chars().take(7).collect::<String>();
        let mut text = format!("dibs: {what} out");
        if let Some(head) = &dibs {
            text.push_str(&format!(
                " (dibs {} here, {} upstream)",
                self.installed.as_deref().map(short).unwrap_or_default(),
                short(head)
            ));
        }
        text.push_str(". Run dibs --update between calls, never in the middle of a batch.\n");
        Some(text)
    }
}

/// The head of the branch `dir` tracks, as its remote says it is now.
fn head_upstream(dir: &Path) -> Option<String> {
    let tracked = git(
        dir,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )?;
    let (remote, branch) = tracked.split_once('/')?;
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["ls-remote", "--heads", remote, branch])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=10",
        )
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let wanted = format!("refs/heads/{branch}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let (head, name) = line.split_once('\t')?;
            (name == wanted).then(|| head.to_string())
        })
}

/// Whether `head` holds work `at` lacks. A head this clone has never fetched is newer; one `at`
/// already contains, as when this computer is ahead of its upstream, is not.
fn newer(dir: &Path, head: &str, at: &str) -> bool {
    !git_ok(dir, &["merge-base", "--is-ancestor", head, at])
}
