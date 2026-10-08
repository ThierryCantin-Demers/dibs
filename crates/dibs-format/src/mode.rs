use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// What a recipe step takes: the lock its record names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lock {
    /// Builds, tests, inspection. Several at once.
    Shared,
    /// The measured run. Nothing else.
    Exclusive,
}

impl Lock {
    pub fn as_str(self) -> &'static str {
        match self {
            Lock::Shared => "shared",
            Lock::Exclusive => "exclusive",
        }
    }
}

/// What a call does on the machine, as its records spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Shared,
    Bench,
    Peek,
    /// A transfer: rsync's end of `dibs --sync`.
    Rsh,
    Status,
    Watch,
    Log,
    Check,
    Out,
    Fetch,
    Kill,
    KillForce,
    Release,
    Gc,
}

impl Mode {
    const ALL: [Mode; 14] = [
        Mode::Shared,
        Mode::Bench,
        Mode::Peek,
        Mode::Rsh,
        Mode::Status,
        Mode::Watch,
        Mode::Log,
        Mode::Check,
        Mode::Out,
        Mode::Fetch,
        Mode::Kill,
        Mode::KillForce,
        Mode::Release,
        Mode::Gc,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Shared => "shared",
            Mode::Bench => "bench",
            Mode::Peek => "peek",
            Mode::Rsh => "rsh",
            Mode::Status => "status",
            Mode::Watch => "watch",
            Mode::Log => "log",
            Mode::Check => "check",
            Mode::Out => "out",
            Mode::Fetch => "fetch",
            Mode::Kill => "kill",
            Mode::KillForce => "kill-force",
            Mode::Release => "release",
            Mode::Gc => "gc",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Mode::ALL
            .into_iter()
            .find(|m| m.as_str() == s)
            .ok_or_else(|| format!("no mode is called {s:?}"))
    }
}
