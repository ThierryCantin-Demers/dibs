use crate::{
    job::PortRange,
    tree::{Clocks, Reflinks},
};
use std::{
    collections::HashMap,
    env, fmt, fs,
    path::{Path, PathBuf},
};

/// A variable's value, where an empty one counts as unset.
pub fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

/// `$HOME`, or `/` where it is unset.
pub fn home() -> PathBuf {
    PathBuf::from(var("HOME").unwrap_or_else(|| "/".into()))
}

/// A name a settings file may hold, and who may set it.
#[derive(Debug, Clone, Copy)]
struct Key {
    name: &'static str,
    setter: Setter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setter {
    /// The machine's file, which every account reads alike, then the environment: one account's
    /// `quick = 600` would send its jobs around everyone's benchmarks.
    Machine,
    /// The account's file, then the machine's, then the environment.
    Account,
    /// The environment alone, which everything that takes the lock on the machine reads it from:
    /// a file that moved it would split who excludes whom.
    Environment,
}

/// Which file a settings file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Whose {
    Machine,
    Account,
}

impl Key {
    const ALL: [Key; 21] = [
        Key::of_setter("bypass", Setter::Machine),
        Key::of_setter("quick", Setter::Machine),
        Key::of_setter("patience", Setter::Machine),
        Key::of_setter("machine_series", Setter::Machine),
        Key::of_setter("peek_warn", Setter::Account),
        Key::of_setter("digest_head", Setter::Account),
        Key::of_setter("digest_tail", Setter::Account),
        Key::of_setter("repeat_window", Setter::Account),
        Key::of_setter("ports", Setter::Account),
        Key::of_setter("idle_after", Setter::Account),
        Key::of_setter("wrote_within", Setter::Account),
        Key::of_setter("keep_days", Setter::Account),
        Key::of_setter("target_keep_days", Setter::Account),
        Key::of_setter("seed_wait", Setter::Account),
        Key::of_setter("no_children", Setter::Account),
        Key::of_setter("lock_dir", Setter::Environment),
        Key::of_setter("shared_lock_dir", Setter::Environment),
        Key::of_setter("shared_state_dir", Setter::Environment),
        Key::of_setter("history", Setter::Environment),
        Key::of_setter("log", Setter::Environment),
        Key::of_setter("scratch", Setter::Environment),
    ];

    const fn of_setter(name: &'static str, setter: Setter) -> Key {
        Key { name, setter }
    }

    /// The key a variable or a file's name gives, as `DIBS_PATIENCE` and `patience` both do.
    fn of(name: &str) -> Option<Key> {
        let name = name
            .strip_prefix("DIBS_")
            .unwrap_or(name)
            .to_ascii_lowercase();
        Key::ALL.into_iter().find(|key| key.name == name)
    }

    fn set_in(&self, whose: Whose) -> bool {
        matches!(
            (self.setter, whose),
            (Setter::Machine, Whose::Machine) | (Setter::Account, _)
        )
    }
}

/// A key a settings file names and may not set, which is ignored.
#[derive(Debug, Clone)]
pub struct Refused {
    file: PathBuf,
    key: Key,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let (file, key) = (self.file.display(), self.key.name);
        match self.key.setter {
            Setter::Environment => write!(
                f,
                "{file} sets {key}, which only the environment sets, since everything that takes the lock here reads it there. It is ignored."
            ),
            Setter::Machine | Setter::Account => write!(
                f,
                "{file} sets {key}, which only {} sets, so that every account here reads the same. It is ignored.",
                SettingsFiles::machine_path().display()
            ),
        }
    }
}

/// A line of a settings file that sets nothing dibs reads: a misspelt key, or a table.
#[derive(Debug, Clone)]
pub struct Unknown {
    file: PathBuf,
    line: String,
}

impl fmt::Display for Unknown {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} holds {:?}, which sets nothing dibs reads",
            self.file.display(),
            self.line
        )
    }
}

/// The machine's settings file and the account's.
struct SettingsFiles {
    machine: SettingsFile,
    account: SettingsFile,
}

impl SettingsFiles {
    fn load() -> SettingsFiles {
        SettingsFiles {
            machine: SettingsFile::load(&SettingsFiles::machine_path(), Whose::Machine),
            account: SettingsFile::load(&SettingsFiles::account_path(), Whose::Account),
        }
    }

    /// A setting by its variable's name, where `DIBS_PATIENCE` is `patience` in a file: the
    /// account's file, then the machine's, then the environment, which ssh forwards nothing to,
    /// so only a call on this computer sets it. What every account must read alike comes from
    /// the machine's file alone, and where the lock and the shared files are from the
    /// environment.
    fn setting(&self, name: &str) -> Option<String> {
        let from_files = match Key::of(name).map(|key| key.setter) {
            Some(Setter::Machine) => self.machine.get(name),
            Some(Setter::Account) => self.account.get(name).or_else(|| self.machine.get(name)),
            Some(Setter::Environment) | None => None,
        };
        from_files.or_else(|| var(name))
    }

    /// `/etc/dibs/runner.toml`, which only root writes and every account reads.
    fn machine_path() -> PathBuf {
        PathBuf::from(
            var("DIBS_MACHINE_SETTINGS").unwrap_or_else(|| "/etc/dibs/runner.toml".into()),
        )
    }

    /// `~/.config/dibs/runner.toml` in the account the runner runs as.
    fn account_path() -> PathBuf {
        var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"))
            .join("dibs/runner.toml")
    }

    fn both(&self) -> impl Iterator<Item = &SettingsFile> {
        [&self.machine, &self.account].into_iter()
    }
}

/// One settings file: flat `name = value` lines, `#` comments, and what it may not set or names
/// to no purpose.
struct SettingsFile {
    values: HashMap<String, String>,
    refused: Vec<Refused>,
    unknown: Vec<Unknown>,
}

impl SettingsFile {
    fn load(path: &Path, whose: Whose) -> SettingsFile {
        let text = fs::read_to_string(path).unwrap_or_default();
        SettingsFile::parse(path, &text, whose)
    }

    fn parse(path: &Path, text: &str, whose: Whose) -> SettingsFile {
        let mut file = SettingsFile {
            values: HashMap::new(),
            refused: Vec::new(),
            unknown: Vec::new(),
        };
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                file.unknown.push(Unknown {
                    file: path.to_path_buf(),
                    line: line.to_string(),
                });
                continue;
            };
            let name = name.trim();
            let value = value.trim();
            let value = match value.strip_prefix('"') {
                Some(quoted) => quoted.split('"').next().unwrap_or_default().to_string(),
                None => value
                    .split(" #")
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            };
            let value = match value.as_str() {
                "true" => "1".to_string(),
                "false" => "0".to_string(),
                _ => value,
            };
            match Key::of(name) {
                Some(key) if key.set_in(whose) => {
                    file.values.insert(key.name.to_string(), value);
                }
                Some(key) => file.refused.push(Refused {
                    file: path.to_path_buf(),
                    key,
                }),
                None => file.unknown.push(Unknown {
                    file: path.to_path_buf(),
                    line: line.to_string(),
                }),
            }
        }
        file
    }

    fn get(&self, name: &str) -> Option<String> {
        let key = Key::of(name)?;
        self.values.get(key.name).filter(|v| !v.is_empty()).cloned()
    }
}

/// The machine's policy: how long a benchmark waits behind quick jobs, how much of a log the
/// digest shows, and the rest.
#[derive(Debug, Clone)]
pub struct Settings {
    /// A quick shared job may go around a queued benchmark.
    pub bypass: bool,
    /// Seconds a queued benchmark lets quick jobs go around it.
    pub patience: u64,
    /// Seconds a job's own history must say it takes, at most, to go around.
    pub quick: u64,
    /// Seconds after which a peek is said to have cost something.
    pub peek_warn: u64,
    pub digest_head: usize,
    pub digest_tail: usize,
    /// Seconds within which the same failure is pointed out.
    pub repeat_window: u64,
    /// The ports `--port` is given from.
    pub ports: PortRange,
    /// Seconds a holder may use no CPU before its status calls it idle.
    pub idle_after: i64,
    /// A holder whose output file was written within so many seconds is working, whatever its
    /// CPU says: a compiler daemon such as sccache works outside the job's tree.
    pub wrote_within: i64,
    /// How long trees, caches, job directories and leftovers are kept unused.
    pub clocks: Clocks,
    /// Seconds a new tree waits for a sibling's build that will leave it more of its lockfile.
    pub seed_wait: u64,
    /// How a seed shares blocks; `DIBS_REFLINK`, read from the environment alone, is a test's.
    pub reflinks: Reflinks,
    /// The machine holds every label's measurements to one card, whoever runs them.
    pub machine_series: bool,
    /// The kernel cannot list a process's children, so the whole table is read instead.
    pub no_children: bool,
    /// What the settings files name and may not set, said on every call until it is gone.
    pub refused: Vec<Refused>,
    /// What they name that nothing reads, which `dibs --check` lists.
    pub unknown: Vec<Unknown>,
}

impl Settings {
    pub fn load() -> Settings {
        let files = SettingsFiles::load();
        let setting = |name: &str| files.setting(name);
        let number = |name: &str, default: u64| {
            setting(name)
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let signed = |name: &str, default: i64| {
            setting(name)
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        Settings {
            idle_after: signed("DIBS_IDLE_AFTER", 60),
            wrote_within: signed("DIBS_WROTE_WITHIN", 120),
            clocks: Clocks {
                keep_days: number("DIBS_KEEP_DAYS", 14),
                target_keep_days: number("DIBS_TARGET_KEEP_DAYS", 5),
            },
            seed_wait: number("DIBS_SEED_WAIT", 900),
            reflinks: Reflinks::of(var("DIBS_REFLINK").as_deref()),
            machine_series: setting("DIBS_MACHINE_SERIES").is_some_and(|v| v == "1"),
            bypass: setting("DIBS_BYPASS").is_none_or(|v| v == "1"),
            patience: number("DIBS_PATIENCE", 60),
            quick: number("DIBS_QUICK", 10),
            peek_warn: number("DIBS_PEEK_WARN", 3),
            digest_head: number("DIBS_DIGEST_HEAD", 20) as usize,
            digest_tail: number("DIBS_DIGEST_TAIL", 20) as usize,
            repeat_window: number("DIBS_REPEAT_WINDOW", 900),
            ports: setting("DIBS_PORTS")
                .and_then(|r| r.parse().ok())
                .unwrap_or_default(),
            no_children: setting("DIBS_NO_CHILDREN").is_some_and(|v| v == "1"),
            refused: files
                .both()
                .flat_map(|file| file.refused.iter().cloned())
                .collect(),
            unknown: files
                .both()
                .flat_map(|file| file.unknown.iter().cloned())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_names_a_setting_as_its_variable_without_the_prefix() {
        let file = SettingsFile::parse(
            Path::new("runner.toml"),
            "# a comment\npatience = 30  # seconds\nports = \"20000-20999\"\nbypass = false\n",
            Whose::Machine,
        );
        assert_eq!(file.get("DIBS_PATIENCE").as_deref(), Some("30"));
        assert_eq!(file.get("DIBS_PORTS").as_deref(), Some("20000-20999"));
        assert_eq!(file.get("DIBS_BYPASS").as_deref(), Some("0"));
        assert_eq!(file.get("DIBS_QUICK"), None);
    }

    #[test]
    fn no_file_can_move_the_lock_or_the_shared_files() {
        for whose in [Whose::Machine, Whose::Account] {
            let file = SettingsFile::parse(
                Path::new("runner.toml"),
                "lock_dir = /elsewhere\nscratch = /big\nhistory = /h\nkeep_days = 5\n",
                whose,
            );
            let refused: Vec<&str> = file.refused.iter().map(|r| r.key.name).collect();
            assert_eq!(refused, ["lock_dir", "scratch", "history"], "{whose:?}");
            assert_eq!(file.get("DIBS_LOCK_DIR"), None);
            assert_eq!(file.get("DIBS_KEEP_DAYS").as_deref(), Some("5"));
        }
    }

    #[test]
    fn what_every_account_must_read_alike_is_the_machines_file_alone() {
        let text = "quick = 600\npatience = 9000\nbypass = true\nmachine_series = false\n";
        let account = SettingsFile::parse(Path::new("runner.toml"), text, Whose::Account);
        let refused: Vec<&str> = account.refused.iter().map(|r| r.key.name).collect();
        assert_eq!(refused, ["quick", "patience", "bypass", "machine_series"]);
        assert_eq!(account.get("DIBS_QUICK"), None);
        let machine = SettingsFile::parse(Path::new("runner.toml"), text, Whose::Machine);
        assert!(machine.refused.is_empty());
        assert_eq!(machine.get("DIBS_QUICK").as_deref(), Some("600"));
    }

    #[test]
    fn a_line_that_sets_nothing_is_kept_to_be_named() {
        let file = SettingsFile::parse(
            Path::new("runner.toml"),
            "[runner]\nquik = 5\nkeep_days = 3\n",
            Whose::Account,
        );
        let unknown: Vec<&str> = file.unknown.iter().map(|u| u.line.as_str()).collect();
        assert_eq!(unknown, ["[runner]", "quik = 5"]);
        assert_eq!(file.get("DIBS_KEEP_DAYS").as_deref(), Some("3"));
    }
}
