//! `dibs --update`, and the change notice: a session is told once when dibs changed under it.

use crate::{caller::Caller, paths::Paths};
use dibs_format::Exit;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime},
};

/// A stamp untouched this long belongs to a session that is gone.
const STAMP_LIFETIME: Duration = Duration::from_secs(31 * 86400);
const LISTED: usize = 10;

/// What this binary was built from, as its build script stamped it.
pub struct Build;

impl Build {
    /// The commit, when it was built from a clone.
    pub const COMMIT: Option<&str> = option_env!("DIBS_COMMIT");

    /// The clone: what `--update` pulls, and where the change notice reads what arrived.
    pub fn clone_dir() -> &'static Path {
        Path::new(env!("DIBS_CLONE"))
    }
}

fn git(clone: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
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
    Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
