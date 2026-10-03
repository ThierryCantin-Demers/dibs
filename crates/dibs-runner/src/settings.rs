use crate::job::PortRange;
use std::{env, path::PathBuf};

/// A variable's value, where an empty one counts as unset.
pub fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

/// `$HOME`, or `/` where it is unset.
pub fn home() -> PathBuf {
    PathBuf::from(var("HOME").unwrap_or_else(|| "/".into()))
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
}

impl Settings {
    pub fn from_env() -> Settings {
        let number =
            |name: &str, default: u64| var(name).and_then(|v| v.parse().ok()).unwrap_or(default);
        let signed =
            |name: &str, default: i64| var(name).and_then(|v| v.parse().ok()).unwrap_or(default);
        Settings {
            idle_after: signed("DIBS_IDLE_AFTER", 60),
            wrote_within: signed("DIBS_WROTE_WITHIN", 120),
            keep_days: number("DIBS_KEEP_DAYS", 14),
            target_keep_days: number("DIBS_TARGET_KEEP_DAYS", 5),
            bypass: var("DIBS_BYPASS").is_none_or(|v| v == "1"),
            patience: number("DIBS_PATIENCE", 60),
            quick: number("DIBS_QUICK", 10),
            peek_warn: number("DIBS_PEEK_WARN", 3),
            digest_head: number("DIBS_DIGEST_HEAD", 20) as usize,
            digest_tail: number("DIBS_DIGEST_TAIL", 20) as usize,
            repeat_window: number("DIBS_REPEAT_WINDOW", 900),
            ports: var("DIBS_PORTS")
                .and_then(|r| r.parse().ok())
                .unwrap_or_default(),
        }
    }
}
