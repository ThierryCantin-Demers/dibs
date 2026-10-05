use crate::job::PortRange;
use std::{
    collections::HashMap,
    env, fmt, fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// A variable's value, where an empty one counts as unset.
pub fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

/// `$HOME`, or `/` where it is unset.
pub fn home() -> PathBuf {
    PathBuf::from(var("HOME").unwrap_or_else(|| "/".into()))
}

/// A machine setting by its variable's name: the machine's settings file, where `DIBS_PATIENCE`
/// is `patience`, then the environment, which ssh forwards nothing to, so only a call on this
/// computer sets it. Where the lock and the shared files are comes from the environment alone.
pub fn setting(name: &str) -> Option<String> {
    let key = Key::of(name);
    let file = match key.map(|key| key.setter) {
        Some(Setter::File) => SettingsFile::once().get(name),
        Some(Setter::Environment) | None => None,
    };
    file.or_else(|| var(name))
}

/// What the settings file names and may not set, each said on every call until it is gone.
pub fn refused() -> &'static [Refused] {
    &SettingsFile::once().refused
}

/// A name the settings file may hold, and who may set it.
#[derive(Debug, Clone, Copy)]
struct Key {
    name: &'static str,
    setter: Setter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setter {
    /// The settings file, then the environment.
    File,
    /// The environment alone, which everything that takes the lock on the machine reads it from:
    /// a file that moved it would split who excludes whom.
    Environment,
}

impl Key {
    const ALL: [Key; 21] = [
        Key::file("bypass"),
        Key::file("quick"),
        Key::file("patience"),
        Key::file("machine_series"),
        Key::file("peek_warn"),
        Key::file("digest_head"),
        Key::file("digest_tail"),
        Key::file("repeat_window"),
        Key::file("ports"),
        Key::file("idle_after"),
        Key::file("wrote_within"),
        Key::file("keep_days"),
        Key::file("target_keep_days"),
        Key::file("seed_wait"),
        Key::file("no_children"),
        Key::environment("lock_dir"),
        Key::environment("shared_lock_dir"),
        Key::environment("shared_state_dir"),
        Key::environment("history"),
        Key::environment("log"),
        Key::environment("scratch"),
    ];

    const fn file(name: &'static str) -> Key {
        Key {
            name,
            setter: Setter::File,
        }
    }

    const fn environment(name: &'static str) -> Key {
        Key {
            name,
            setter: Setter::Environment,
        }
    }

    /// The key a variable or a file's name gives, as `DIBS_PATIENCE` and `patience` both do.
    fn of(name: &str) -> Option<Key> {
        let name = name
            .strip_prefix("DIBS_")
            .unwrap_or(name)
            .to_ascii_lowercase();
        Key::ALL.into_iter().find(|key| key.name == name)
    }
}

/// A key the settings file names and may not set, which is ignored.
#[derive(Debug, Clone)]
pub struct Refused {
    file: PathBuf,
    key: &'static str,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} sets {}, which only the environment sets, since everything that takes the lock here reads it there. It is ignored.",
            self.file.display(),
            self.key
        )
    }
}

/// `~/.config/dibs/machine.toml` in the account the runner runs as: flat `name = value` lines.
struct SettingsFile {
    values: HashMap<String, String>,
    refused: Vec<Refused>,
}

impl SettingsFile {
    fn once() -> &'static SettingsFile {
        static FILE: OnceLock<SettingsFile> = OnceLock::new();
        FILE.get_or_init(SettingsFile::load)
    }

    fn load() -> SettingsFile {
        let dir = var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"));
        let path = dir.join("dibs/machine.toml");
        let text = fs::read_to_string(&path).unwrap_or_default();
        SettingsFile::parse(&path, &text)
    }

    fn parse(path: &Path, text: &str) -> SettingsFile {
        let mut file = SettingsFile {
            values: HashMap::new(),
            refused: Vec::new(),
        };
        for line in text.lines() {
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name.starts_with('#') || name.is_empty() {
                continue;
            }
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
                Some(key) if key.setter == Setter::Environment => file.refused.push(Refused {
                    file: path.to_path_buf(),
                    key: key.name,
                }),
                _ => {
                    file.values.insert(name.to_string(), value);
                }
            }
        }
        file
    }

    fn get(&self, name: &str) -> Option<String> {
        let key = name
            .strip_prefix("DIBS_")
            .unwrap_or(name)
            .to_ascii_lowercase();
        self.values.get(&key).filter(|v| !v.is_empty()).cloned()
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
    /// Days a worktree, a job's directory or a temporary file is kept unused.
    pub keep_days: u64,
    /// Days a build cache is kept unused: a compiler refills it, which a worktree is not.
    pub target_keep_days: u64,
    /// The machine holds every label's measurements to one card, whoever runs them.
    pub machine_series: bool,
}

impl Settings {
    pub fn load() -> Settings {
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
            keep_days: number("DIBS_KEEP_DAYS", 14),
            target_keep_days: number("DIBS_TARGET_KEEP_DAYS", 5),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_names_a_setting_as_its_variable_without_the_prefix() {
        let file = SettingsFile::parse(
            Path::new("machine.toml"),
            "# a comment\npatience = 30  # seconds\nports = \"20000-20999\"\nbypass = false\n",
        );
        assert_eq!(file.get("DIBS_PATIENCE").as_deref(), Some("30"));
        assert_eq!(file.get("DIBS_PORTS").as_deref(), Some("20000-20999"));
        assert_eq!(file.get("DIBS_BYPASS").as_deref(), Some("0"));
        assert_eq!(file.get("DIBS_QUICK"), None);
    }

    #[test]
    fn the_file_cannot_move_the_lock_or_the_shared_files() {
        let file = SettingsFile::parse(
            Path::new("machine.toml"),
            "lock_dir = /elsewhere\nscratch = /big\nhistory = /h\nquick = 5\n",
        );
        let refused: Vec<&str> = file.refused.iter().map(|r| r.key).collect();
        assert_eq!(refused, ["lock_dir", "scratch", "history"]);
        assert_eq!(file.get("DIBS_LOCK_DIR"), None);
        assert_eq!(file.get("DIBS_QUICK").as_deref(), Some("5"));
    }
}
