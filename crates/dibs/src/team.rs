//! The team clone: the repo a team shares its recipes and machines through, which each member
//! clones once, with `recipes/<repo>.toml` and `machines.toml` in it.

use crate::{
    git::Git,
    inventory::Inventory,
    machine::{Fleet, Named, Ssh, Target, Unreachable},
    paths::Paths,
    recipe::RecipeDirs,
};
use dibs_format::{Exit, MachineName};
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Where `Paths::team` puts the clone.
pub struct Team {
    pub dir: PathBuf,
}

/// One of the team's machines as `--join` tried it.
struct Tried {
    name: MachineName,
    /// Why it would not answer; None when it did.
    refused: Option<String>,
}

impl Team {
    pub fn here() -> Option<Team> {
        Paths::from_env().team().map(|dir| Team { dir })
    }

    /// Whether a file is the team's, so a change written to it is said to need a push.
    pub fn holds(&self, file: &Path) -> bool {
        file.starts_with(&self.dir)
    }

    /// What to say after writing `file`, when the change reaches nobody until it is pushed.
    pub fn push_note(file: &Path) -> Option<String> {
        Team::here().filter(|team| team.holds(file)).map(|_| {
            format!(
                "  {} is the team's: commit and push it, so everyone has the change.",
                file.display()
            )
        })
    }

    /// `dibs --join <repo>`: the clone, what it brings, and which of its machines will not take
    /// this person yet.
    pub fn join(&self, repo: &str) -> i32 {
        if self.dir.exists() {
            let origin = Git(&self.dir)
                .run(&["remote", "get-url", "origin"])
                .map(|o| format!(", of {o}"))
                .unwrap_or_default();
            eprintln!(
                "dibs: {} is already a team clone{origin}. To join another, move it away first.",
                self.dir.display()
            );
            return Exit::Refused.code().into();
        }
        let url = Team::url(repo);
        if let Some(parent) = self.dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let cloned = Command::new("git")
            .args(["clone", "--quiet", &url])
            .arg(&self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .output();
        let said = match cloned {
            Ok(out) if out.status.success() => None,
            Ok(out) => Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Err(e) => Some(format!("git could not run: {e}")),
        };
        if let Some(said) = said {
            eprintln!("dibs: could not clone {url}:\n  {said}");
            if url.starts_with("https://") {
                eprintln!(
                    "  A private repo needs git to have your login. For GitHub:  gh auth setup-git"
                );
            }
            return Exit::Failed.code().into();
        }
        print!("{}", self.joined(&url));
        let fleet = match Fleet::load(Paths::from_env().inventory()) {
            Ok(fleet) => fleet,
            Err(e) => {
                eprintln!("dibs: {e}");
                return Exit::Failed.code().into();
            }
        };
        print!("{}", Team::reach(&fleet, &self.machines()));
        0
    }

    /// `owner/name` is a GitHub repo; anything else is given to git as it is.
    fn url(repo: &str) -> String {
        let github = repo.split('/').count() == 2
            && !repo.contains(':')
            && !repo.starts_with(['.', '/', '~'])
            && !Path::new(repo).exists();
        match github {
            true => format!("https://github.com/{repo}.git"),
            false => repo.to_string(),
        }
    }

    /// The team's own machines, from its file alone.
    fn machines(&self) -> Vec<MachineName> {
        Team::names_in(&self.dir.join("machines.toml"))
    }

    fn names_in(file: &Path) -> Vec<MachineName> {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|text| Inventory::parse(&text).ok())
            .map(|i| i.names().cloned().collect())
            .unwrap_or_default()
    }

    /// What the clone brought, and what of this person's own will be used in its place.
    fn joined(&self, url: &str) -> String {
        let machines = self.machines();
        let dirs = RecipeDirs::here();
        let repos = RecipeDirs::repos_in(&dirs.team);
        let list = |names: &[String]| match names.is_empty() {
            true => "none".to_string(),
            false => names.join(", "),
        };
        let mut text = format!("joined {url}, cloned in {}\n", self.dir.display());
        let named: Vec<String> = machines.iter().map(MachineName::to_string).collect();
        let _ = writeln!(text, "  machines: {}", list(&named));
        let _ = writeln!(text, "  recipes:  {}", list(&repos));
        text.push_str(&self.shadowed());
        let mine: BTreeSet<String> = RecipeDirs::repos_in(&dirs.own).into_iter().collect();
        let overridden: Vec<&String> = repos.iter().filter(|r| mine.contains(*r)).collect();
        if !overridden.is_empty() {
            let _ = writeln!(
                text,
                "  {} has recipes of its own for {}, which override the team's: remove them to follow the team's.",
                dirs.own.display(),
                overridden
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        text
    }

    /// The team's machines this person's own inventory also has, whose entries win.
    fn shadowed(&self) -> String {
        let own = Team::names_in(&self.dir.with_file_name("machines.toml"));
        let shadowed: Vec<String> = self
            .machines()
            .iter()
            .filter(|m| own.contains(m))
            .map(MachineName::to_string)
            .collect();
        match shadowed.as_slice() {
            [] => String::new(),
            [one] => format!(
                "  Your own machines.toml also has {one}, used in place of the team's: `dibs --forget {one}` follows the team's.\n"
            ),
            many => format!(
                "  Your own machines.toml also has {}, used in place of the team's: `dibs --forget <machine>` on each follows the team's.\n",
                many.join(", ")
            ),
        }
    }

    /// Each machine tried at once, and what stops the ones that do not answer.
    fn reach(fleet: &Fleet, names: &[MachineName]) -> String {
        let tried: Vec<Tried> = std::thread::scope(|scope| {
            let trying: Vec<_> = names
                .iter()
                .map(|name| scope.spawn(move || Team::try_one(fleet, name)))
                .collect();
            trying.into_iter().filter_map(|t| t.join().ok()).collect()
        });
        let mut text = String::new();
        if tried.is_empty() {
            return text;
        }
        text.push_str("  its machines, tried over ssh:\n");
        for t in &tried {
            let answer = match t.refused {
                None => "answers",
                Some(_) => "cannot be reached yet, as below",
            };
            let _ = writeln!(text, "    {}: {answer}", t.name);
        }
        for refused in tried.iter().filter_map(|t| t.refused.as_deref()) {
            text.push_str(refused);
        }
        text
    }

    fn try_one(fleet: &Fleet, name: &MachineName) -> Tried {
        let mut target = Target::literal("");
        if target.go_to(fleet, name.as_str(), Named::On).is_err() {
            return Tried {
                name: name.clone(),
                refused: Some(format!("dibs: {name} has no ssh string to reach it by.\n")),
            };
        }
        let out = Ssh::to(target.ssh_config.as_deref())
            .args(Ssh::options())
            .args([&target.host, "true"])
            .stdin(Stdio::null())
            .output();
        let refused = match out {
            Ok(out) if out.status.success() => None,
            Ok(out) => Some(
                Unreachable {
                    target: &target,
                    said: String::from_utf8_lossy(&out.stderr).into_owned(),
                }
                .diagnosis(),
            ),
            Err(e) => Some(format!("dibs: ssh could not run: {e}\n")),
        };
        Tried {
            name: name.clone(),
            refused,
        }
    }

    /// Moves a clone of the team's repo out of where this person's own recipes go, where it was
    /// cloned when it held recipes alone, once it has been pulled to the team's layout.
    pub fn settle(&self, own: &Path) {
        let moves = own.parent() == self.dir.parent()
            && !self.dir.exists()
            && own.join(".git").exists()
            && own.join("recipes").is_dir();
        if !moves {
            return;
        }
        match std::fs::rename(own, &self.dir) {
            Ok(()) => eprint!(
                "dibs: {} holds the team's machines as well as its recipes now, so it moved to {}.\n{}",
                own.display(),
                self.dir.display(),
                self.shadowed()
            ),
            Err(e) => eprintln!(
                "dibs: {} is the team's clone, and could not move to {}: {e}",
                own.display(),
                self.dir.display()
            ),
        }
    }

    /// `settle` for this computer's own paths, before a call reads recipes or machines.
    pub fn settle_here() {
        if let Some(team) = Team::here() {
            team.settle(&RecipeDirs::here().own);
        }
    }
}
