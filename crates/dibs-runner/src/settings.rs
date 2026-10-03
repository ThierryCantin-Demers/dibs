use crate::job::PortRange;
use std::{collections::HashMap, env, fs, path::PathBuf, sync::OnceLock};

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
/// computer sets it.
pub fn setting(name: &str) -> Option<String> {
    static FILE: OnceLock<SettingsFile> = OnceLock::new();
    FILE.get_or_init(SettingsFile::load)
        .get(name)
        .or_else(|| var(name))
}

/// `~/.config/dibs/machine.toml` in the account the runner runs as: flat `name = value` lines.
struct SettingsFile {
    values: HashMap<String, String>,
}

impl SettingsFile {
    fn load() -> SettingsFile {
        let dir = var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"));
        let text = fs::read_to_string(dir.join("dibs/machine.toml")).unwrap_or_default();
        SettingsFile {
            values: SettingsFile::parse(&text),
        }
    }

    fn parse(text: &str) -> HashMap<String, String> {
        text.lines()
            .filter_map(|line| {
                let (name, value) = line.split_once('=')?;
                let name = name.trim();
                let value = value.trim();
                let value = match value.strip_prefix('"') {
                    Some(quoted) => quoted.split('"').next()?.to_string(),
                    None => value.split(" #").next()?.trim().to_string(),
                };
                let value = match value.as_str() {
                    "true" => "1".to_string(),
                    "false" => "0".to_string(),
                    _ => value,
                };
                (!name.starts_with('#') && !name.is_empty()).then(|| (name.to_string(), value))
            })
            .collect()
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
        let file = SettingsFile {
            values: SettingsFile::parse(
                "# a comment\npatience = 30  # seconds\nports = \"20000-20999\"\nbypass = false\n",
            ),
        };
        assert_eq!(file.get("DIBS_PATIENCE").as_deref(), Some("30"));
        assert_eq!(file.get("DIBS_PORTS").as_deref(), Some("20000-20999"));
        assert_eq!(file.get("DIBS_BYPASS").as_deref(), Some("0"));
        assert_eq!(file.get("DIBS_QUICK"), None);
    }
}
