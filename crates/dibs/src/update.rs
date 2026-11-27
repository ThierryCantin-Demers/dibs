//! `dibs --update`, and the two notices: a session is told once when dibs changed under it, and
//! once when its upstream has a newer dibs or newer recipes than this computer runs.

use crate::{caller::Caller, git::Git, paths::Paths, team::Team};
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
    /// Whether to refresh the agent plugin in each harness that has it installed.
    pub plugin: bool,
    /// Whether to fetch the team's machines from the dibspatcher this computer names.
    pub team: bool,
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
            plugin: true,
            team: true,
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
        match self
            .team
            .then(|| Team::at(&Paths::from_env()).refresh())
            .flatten()
        {
            Some(Ok(count)) => {
                let _ = writeln!(out, "the team's machines: {count}, from the dibspatcher");
            }
            Some(Err(why)) => {
                let _ = writeln!(
                    err,
                    "dibs: the team's machines were not refreshed, and the copy here stays: {why}"
                );
            }
            None => {}
        }
        if self.plugin {
            refresh_plugin(out);
        }
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

/// The plugin's rules are a copy each agent harness keeps, so they follow an update only when it
/// is told to fetch them. A harness that is absent, or lacks the plugin, has nothing to refresh.
fn refresh_plugin(out: &mut impl Write) {
    for harness in Harness::ALL {
        harness.refresh(out);
    }
}

#[derive(Clone, Copy)]
enum Harness {
    Claude,
    Codex,
}

type Step = &'static [&'static str];

const PLUGIN: &str = "dibs@dibs";

impl Harness {
    const ALL: [Harness; 2] = [Harness::Claude, Harness::Codex];

    fn refresh(self, out: &mut impl Write) {
        let (name, program) = match self {
            Harness::Claude => ("Claude Code", "claude"),
            Harness::Codex => ("Codex", "codex"),
        };
        let steps = match self.steps() {
            Ok(Some(steps)) => steps,
            Ok(None) => return,
            Err(why) => {
                let _ = writeln!(
                    out,
                    "{name} plugin {PLUGIN} was not refreshed, since its plugins could not be listed: {why}"
                );
                return;
            }
        };
        let refreshed = steps.iter().all(|step| self.run(step).is_some());
        let _ = match refreshed {
            true => writeln!(
                out,
                "{name} plugin {PLUGIN} refreshed; sessions started from now on read it"
            ),
            false => {
                let retry: Vec<String> = steps
                    .iter()
                    .map(|step| format!("{program} {}", step.join(" ")))
                    .collect();
                writeln!(
                    out,
                    "{name} plugin {PLUGIN} could not be refreshed:  {}",
                    retry.join(" && ")
                )
            }
        };
    }

    /// The calls that refresh the plugin here, none when the harness or the plugin is absent, and
    /// what the harness said when its listing failed.
    fn steps(self) -> Result<Option<Vec<Step>>, String> {
        Ok(match self {
            Harness::Claude => self.list(&["plugin", "list"])?.and_then(|listing| {
                listing.contains(PLUGIN).then(|| {
                    vec![
                        &["plugin", "marketplace", "update", "dibs"][..],
                        &["plugin", "update", PLUGIN][..],
                    ]
                })
            }),
            // Scoped, since one broken marketplace fails the whole listing. A marketplace that
            // was never added lists nothing and succeeds.
            Harness::Codex => self
                .list(&["plugin", "list", "--marketplace", "dibs", "--json"])?
                .and_then(|listing| codex_steps(&listing)),
        })
    }

    /// The listing, none when the harness is not installed, or its first line of complaint.
    fn list(self, args: &[&str]) -> Result<Option<String>, String> {
        let Ok(listed) = self.command(args).output() else {
            return Ok(None);
        };
        if !listed.status.success() {
            let stderr = String::from_utf8_lossy(&listed.stderr);
            let why = stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no reason given");
            return Err(why.trim().to_string());
        }
        Ok(Some(String::from_utf8_lossy(&listed.stdout).into_owned()))
    }

    fn run(self, args: &[&str]) -> Option<String> {
        self.command(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }

    fn command(self, args: &[&str]) -> Command {
        let program = match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        };
        let mut command = Command::new(program);
        command.args(args).stdin(std::process::Stdio::null());
        command
    }
}

/// Codex copies the plugin into its cache only when it is added, and adding enables it, so a
/// disabled plugin is left alone. It upgrades only a marketplace it cloned, and refuses one added
/// from a local checkout.
fn codex_steps(listing: &str) -> Option<Vec<Step>> {
    let listing: serde_json::Value = serde_json::from_str(listing).ok()?;
    let plugin = listing["installed"]
        .as_array()?
        .iter()
        .find(|p| p["pluginId"] == PLUGIN)?;
    if plugin["enabled"] != true {
        return None;
    }
    let upgrade: Step = &["plugin", "marketplace", "upgrade", "dibs"];
    let add: Step = &["plugin", "add", PLUGIN];
    Some(match plugin["marketplaceSource"]["sourceType"] == "local" {
        true => vec![add],
        false => vec![upgrade, add],
    })
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
        text.push_str(
            "  Flags and output you remember may be wrong now. Read dibs --help --all, and\n",
        );
        text.push_str(&format!(
            "  {}/dibs-agent-rules.md for how it is meant to be used.\n",
            clone.display()
        ));
        text
    }
}

/// What the upstreams of the clone and of the recipes say their heads are, against what runs
/// here. It is asked about once an hour, beside a call and never waited for, so a slow or absent
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

/// What the upstreams said, as kept in the cache: each head with the checkout it was asked for,
/// since a head means nothing to another checkout.
#[derive(Default, PartialEq, Debug)]
struct Heads {
    asked: u64,
    /// Each upstream, as `Tracked::key` names it, and its head.
    heads: Vec<(String, String)>,
}

impl Heads {
    fn read(path: &Path) -> Heads {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut heads = Heads::default();
        for line in text.lines() {
            match line.split_once(' ') {
                Some(("asked", at)) => heads.asked = at.parse().unwrap_or(0),
                Some(("head", rest)) => {
                    if let Some((head, key)) = rest.split_once(' ') {
                        heads.heads.push((key.to_string(), head.to_string()));
                    }
                }
                _ => {}
            }
        }
        heads
    }

    /// The head last asked for the upstream `dir` tracks now.
    fn of(&self, dir: &Path) -> Option<&str> {
        let key = Tracked::of(dir)?.key(dir);
        self.heads
            .iter()
            .find(|(at, _)| *at == key)
            .map(|(_, head)| head.as_str())
    }

    /// Written whole and renamed into place, so a call that ends mid-write leaves the old answer.
    fn write(&self, path: &Path) -> bool {
        let mut text = format!("asked {}\n", self.asked);
        for (key, head) in &self.heads {
            text.push_str(&format!("head {head} {key}\n"));
        }
        let Some(dir) = path.parent() else {
            return false;
        };
        let partial = dir.join(format!(".upstream.{}", std::process::id()));
        std::fs::create_dir_all(dir).is_ok()
            && std::fs::write(&partial, text).is_ok()
            && std::fs::rename(&partial, path).is_ok()
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
        if upstream.due(epoch_now())
            && let Ok(exe) = std::env::current_exe()
        {
            // A process of its own, so a call that ends first does not end the ask with it, and
            // with no stream of the caller's, so nothing waits on it.
            let _ = Command::new(exe)
                .arg(Upstream::WORD)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }

    /// The word a call starts its ask under.
    pub const WORD: &str = "__ask-upstream";

    /// The ask a call started, which keeps its answer and ends.
    pub fn serve() {
        let paths = Paths::from_env();
        if let Some(cache) = paths.upstream() {
            Upstream {
                cache,
                seen: PathBuf::new(),
                clone: Build::clone_dir().to_path_buf(),
                installed: None,
                recipes: paths.recipes(),
            }
            .ask(epoch_now());
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
        // A cache that cannot keep the ask could not keep its answer either, so every call
        // would ask again.
        heads.write(&self.cache)
    }

    /// Asks both upstreams for their heads and keeps the answer.
    pub fn ask(&self, now: u64) {
        let heads = std::iter::once(self.clone.as_path())
            .chain(self.recipes.as_deref())
            .filter_map(|dir| {
                let tracked = Tracked::of(dir)?;
                Some((tracked.key(dir), tracked.head(dir)?))
            })
            .collect();
        Heads { asked: now, heads }.write(&self.cache);
    }

    /// What to tell `caller`, the first time the last answer names something newer.
    pub fn notice(&self, caller: &Caller) -> Option<String> {
        let heads = Heads::read(&self.cache);
        let dibs = heads
            .of(&self.clone)
            .filter(|head| {
                self.installed
                    .as_deref()
                    .is_some_and(|at| newer(&self.clone, head, at))
            })
            .map(str::to_string);
        let recipes = self
            .recipes
            .as_deref()
            .and_then(|dir| heads.of(dir).filter(|head| newer(dir, head, "HEAD")))
            .map(str::to_string);
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

/// The remote and the ref on it that a checkout's branch pulls from, as its config says.
struct Tracked {
    remote: String,
    merge: String,
}

impl Tracked {
    fn of(dir: &Path) -> Option<Tracked> {
        let branch = git(dir, &["symbolic-ref", "--short", "-q", "HEAD"])?;
        let remote = git(dir, &["config", &format!("branch.{branch}.remote")])?;
        let merge = git(dir, &["config", &format!("branch.{branch}.merge")])?;
        (remote != ".").then_some(Tracked { remote, merge })
    }

    /// What the cache files this upstream's head under: the same checkout pulling from another
    /// branch or remote asks anew.
    fn key(&self, dir: &Path) -> String {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        format!("{} {} {}", self.remote, self.merge, dir.display())
    }

    /// The head of the tracked ref, as the remote says it is now.
    fn head(&self, dir: &Path) -> Option<String> {
        let mut ls = Command::new("git");
        ls.arg("-C")
            .arg(dir)
            .args(["ls-remote", &self.remote, &self.merge])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
            .env("GIT_HTTP_LOW_SPEED_TIME", "10");
        // An ssh the repository or the person set is how its remote is reached at all.
        let configured = ["GIT_SSH_COMMAND", "GIT_SSH"]
            .iter()
            .any(|var| std::env::var_os(var).is_some())
            || git(dir, &["config", "core.sshCommand"]).is_some();
        if !configured {
            ls.env(
                "GIT_SSH_COMMAND",
                "ssh -o BatchMode=yes -o ConnectTimeout=10",
            );
        }
        let out = ls.output().ok().filter(|out| out.status.success())?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|line| {
                let (head, name) = line.split_once('\t')?;
                (name == self.merge).then(|| head.to_string())
            })
    }
}

/// Whether `head` holds work `at` lacks. A head this clone has never fetched is newer; one `at`
/// already contains, as when this computer is ahead of its upstream, is not.
fn newer(dir: &Path, head: &str, at: &str) -> bool {
    // A partial clone would otherwise fetch a missing head here, before the call's own work.
    let contained = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", "--is-ancestor", head, at])
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    !contained
}

#[cfg(test)]
mod tests {
    use super::codex_steps;

    fn listed(enabled: bool, source: &str) -> String {
        format!(
            r#"{{"installed": [
                {{"pluginId": "dibs@dibs-fork", "enabled": true, "marketplaceSource": {{"sourceType": "git"}}}},
                {{"pluginId": "dibs@dibs", "enabled": {enabled}, "marketplaceSource": {{"sourceType": "{source}"}}}}
            ], "available": []}}"#
        )
    }

    #[test]
    fn codex_upgrades_a_cloned_marketplace_before_adding_the_plugin_again() {
        let steps = codex_steps(&listed(true, "git")).unwrap();
        assert_eq!(
            steps.iter().map(|s| s[1]).collect::<Vec<_>>(),
            ["marketplace", "add"]
        );
    }

    #[test]
    fn codex_adds_the_plugin_from_a_local_marketplace_without_upgrading_it() {
        let steps = codex_steps(&listed(true, "local")).unwrap();
        assert_eq!(steps.iter().map(|s| s[1]).collect::<Vec<_>>(), ["add"]);
    }

    #[test]
    fn codex_leaves_a_disabled_or_absent_plugin_alone() {
        assert!(codex_steps(&listed(false, "git")).is_none());
        assert!(
            codex_steps(r#"{"installed": [], "available": [{"pluginId": "dibs@dibs"}]}"#).is_none()
        );
        assert!(codex_steps("not json").is_none());
    }
}
