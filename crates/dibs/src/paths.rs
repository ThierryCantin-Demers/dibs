//! Every file dibs keeps on this computer, in one place.
//!
//! Each follows its `DIBS_*` variable when one is set, then the XDG base directory, then the
//! directory under `$HOME` the XDG specification names. An empty variable counts as unset, as it
//! does in a shell's `${VAR:-default}`.

use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

/// The variables a path is read from, captured once.
#[derive(Debug, Clone, Default)]
pub struct Paths {
    vars: BTreeMap<&'static str, PathBuf>,
}

/// The stamps that keep a report from being fetched or shown twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportsStamp {
    /// When replies were last asked for.
    Asked,
    /// Replies already printed to this session.
    Shown,
    /// Reports the listener already woke for.
    Woken,
}

impl ReportsStamp {
    fn file(self) -> &'static str {
        match self {
            ReportsStamp::Asked => "reports-asked",
            ReportsStamp::Shown => "reports-shown",
            ReportsStamp::Woken => "reports-woken",
        }
    }
}

impl Paths {
    const VARS: [&'static str; 14] = [
        "HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "DIBS_RUNS",
        "DIBS_FRICTION",
        "DIBS_SERIES",
        "DIBS_SEEN",
        "DIBS_KEPT",
        "DIBS_ROUTE_DOWN",
        "DIBS_MACHINES",
        "DIBS_RECIPES",
        "DIBS_FLEET",
        "DIBS_SCRATCH",
    ];

    pub fn from_env() -> Paths {
        Paths::from_vars(|name| std::env::var_os(name))
    }

    pub fn from_vars(read: impl Fn(&str) -> Option<OsString>) -> Paths {
        let vars = Paths::VARS
            .iter()
            .filter_map(|name| {
                let value = read(name).filter(|v| !v.is_empty())?;
                Some((*name, PathBuf::from(value)))
            })
            .collect();
        Paths { vars }
    }

    /// Every recipe run, one JSON line each.
    pub fn runs(&self) -> Option<PathBuf> {
        self.overridden("DIBS_RUNS", self.state("runs.jsonl"))
    }

    /// What got in the way, one JSON line each.
    pub fn friction(&self) -> Option<PathBuf> {
        self.overridden("DIBS_FRICTION", self.state("friction.jsonl"))
    }

    /// Which card each label's series is on, per machine.
    pub fn series(&self) -> Option<PathBuf> {
        self.overridden("DIBS_SERIES", self.state("series"))
    }

    /// Which machine last built each repo, so its work goes back there.
    pub fn affinity(&self) -> Option<PathBuf> {
        self.state("affinity")
    }

    /// When each session last saw which commit of dibs, for the change notice.
    pub fn seen(&self) -> Option<PathBuf> {
        self.overridden("DIBS_SEEN", self.state("seen"))
    }

    /// Job logs already read, kept here so they answer once the machine is gone.
    pub fn kept_jobs(&self) -> Option<PathBuf> {
        self.overridden("DIBS_KEPT", self.state("jobs"))
    }

    /// One directory per batch, with each step's output and the summary.
    pub fn batches(&self) -> Option<PathBuf> {
        self.state("batch")
    }

    /// Machines that did not answer, kept out of placement for a while.
    pub fn route_down(&self) -> Option<PathBuf> {
        self.overridden("DIBS_ROUTE_DOWN", self.state("route-down"))
    }

    pub fn reports(&self, stamp: ReportsStamp) -> Option<PathBuf> {
        self.state(stamp.file())
    }

    /// Replies fetched in the background, one file per session, printed on its next call.
    pub fn reports_news(&self) -> Option<PathBuf> {
        self.state("reports-news")
    }

    /// Commits checked out here to be sent, one tree per repo identity.
    pub fn sent(&self) -> Option<PathBuf> {
        self.cache("sent")
    }

    /// What each repo's remotes answered, so a ref is not looked up on every call.
    pub fn remotes(&self) -> Option<PathBuf> {
        self.cache("remotes")
    }

    /// `machines.toml`, the inventory.
    pub fn inventory(&self) -> Option<PathBuf> {
        self.overridden("DIBS_MACHINES", self.config("machines.toml"))
    }

    /// Recipe files kept in local config, `<repo>.toml` each.
    pub fn recipes(&self) -> Option<PathBuf> {
        self.overridden("DIBS_RECIPES", self.config("recipes"))
    }

    /// `fleet.toml`, what each machine should have.
    pub fn fleet(&self) -> Option<PathBuf> {
        self.overridden("DIBS_FLEET", self.config("fleet.toml"))
    }

    /// Where a call keeps files for a moment: the scratch a machine's jobs use, so on a machine
    /// both are one directory.
    pub fn scratch(&self) -> Option<PathBuf> {
        let home = self.vars.get("HOME").map(|home| home.join(".cache/dibs"));
        self.overridden("DIBS_SCRATCH", home)
    }

    fn overridden(&self, var: &str, default: Option<PathBuf>) -> Option<PathBuf> {
        self.vars.get(var).cloned().or(default)
    }

    /// `$<xdg>/dibs/<name>`, or `$HOME/<under_home>/dibs/<name>`.
    fn under(&self, xdg: &str, under_home: &str, name: &str) -> Option<PathBuf> {
        let base = match self.vars.get(xdg) {
            Some(base) => base.clone(),
            None => self.vars.get("HOME")?.join(under_home),
        };
        Some(base.join("dibs").join(name))
    }

    fn state(&self, name: &str) -> Option<PathBuf> {
        self.under("XDG_STATE_HOME", ".local/state", name)
    }

    fn cache(&self, name: &str) -> Option<PathBuf> {
        self.under("XDG_CACHE_HOME", ".cache", name)
    }

    fn config(&self, name: &str) -> Option<PathBuf> {
        self.under("XDG_CONFIG_HOME", ".config", name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(vars: &[(&str, &str)]) -> Paths {
        let vars: BTreeMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        Paths::from_vars(|name| vars.get(name).cloned())
    }

    #[test]
    fn every_file_is_under_home_when_nothing_else_is_said() {
        let p = paths(&[("HOME", "/h")]);
        let state = |name: &str| Some(PathBuf::from("/h/.local/state/dibs").join(name));
        assert_eq!(p.runs(), state("runs.jsonl"));
        assert_eq!(p.friction(), state("friction.jsonl"));
        assert_eq!(p.series(), state("series"));
        assert_eq!(p.affinity(), state("affinity"));
        assert_eq!(p.seen(), state("seen"));
        assert_eq!(p.kept_jobs(), state("jobs"));
        assert_eq!(p.batches(), state("batch"));
        assert_eq!(p.route_down(), state("route-down"));
        assert_eq!(p.reports(ReportsStamp::Woken), state("reports-woken"));
        assert_eq!(p.sent(), Some("/h/.cache/dibs/sent".into()));
        assert_eq!(p.remotes(), Some("/h/.cache/dibs/remotes".into()));
        assert_eq!(p.inventory(), Some("/h/.config/dibs/machines.toml".into()));
        assert_eq!(p.recipes(), Some("/h/.config/dibs/recipes".into()));
        assert_eq!(p.fleet(), Some("/h/.config/dibs/fleet.toml".into()));
    }

    #[test]
    fn the_xdg_directories_move_every_file_under_them() {
        let p = paths(&[
            ("HOME", "/h"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CACHE_HOME", "/c"),
            ("XDG_CONFIG_HOME", "/g"),
        ]);
        assert_eq!(p.runs(), Some("/s/dibs/runs.jsonl".into()));
        assert_eq!(p.friction(), Some("/s/dibs/friction.jsonl".into()));
        assert_eq!(p.affinity(), Some("/s/dibs/affinity".into()));
        assert_eq!(p.sent(), Some("/c/dibs/sent".into()));
        assert_eq!(p.inventory(), Some("/g/dibs/machines.toml".into()));
    }

    #[test]
    fn a_variable_of_its_own_wins_and_an_empty_one_counts_as_unset() {
        let p = paths(&[
            ("HOME", "/h"),
            ("DIBS_RUNS", "/r.jsonl"),
            ("DIBS_MACHINES", "/m.toml"),
            ("XDG_STATE_HOME", ""),
            ("DIBS_FRICTION", ""),
        ]);
        assert_eq!(p.runs(), Some("/r.jsonl".into()));
        assert_eq!(p.inventory(), Some("/m.toml".into()));
        assert_eq!(
            p.friction(),
            Some("/h/.local/state/dibs/friction.jsonl".into())
        );
    }

    #[test]
    fn without_home_only_what_was_named_is_known() {
        let p = paths(&[("DIBS_SERIES", "/x")]);
        assert_eq!(p.series(), Some("/x".into()));
        assert_eq!(p.runs(), None);
    }
}
