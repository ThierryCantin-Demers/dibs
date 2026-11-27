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
///
/// # Guarantees
///
/// - `from_str` reads back every name `as_str` writes, and refuses any other.
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
    /// Waits on a job its call let go of, until it ends or checks in again.
    Follow,
    Fetch,
    Kill,
    KillForce,
    Release,
    Gc,
    /// Taken back by its owners: nothing starts until it is opened.
    Close,
    Open,
}

impl Mode {
    const ALL: [Mode; 17] = [
        Mode::Shared,
        Mode::Bench,
        Mode::Peek,
        Mode::Rsh,
        Mode::Status,
        Mode::Watch,
        Mode::Log,
        Mode::Check,
        Mode::Out,
        Mode::Follow,
        Mode::Fetch,
        Mode::Kill,
        Mode::KillForce,
        Mode::Release,
        Mode::Gc,
        Mode::Close,
        Mode::Open,
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
            Mode::Follow => "follow",
            Mode::Fetch => "fetch",
            Mode::Kill => "kill",
            Mode::KillForce => "kill-force",
            Mode::Release => "release",
            Mode::Gc => "gc",
            Mode::Close => "close",
            Mode::Open => "open",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_reads_back_from_its_name() {
        for mode in Mode::ALL {
            assert_eq!(mode.as_str().parse::<Mode>(), Ok(mode));
        }
    }

    #[test]
    fn a_name_no_mode_has_is_refused() {
        assert!("sprint".parse::<Mode>().is_err());
    }

    /// Stops compiling when a variant is added, until it is counted here and listed in `ALL`.
    #[test]
    fn every_variant_is_in_all() {
        let counted = |mode: Mode| match mode {
            Mode::Shared
            | Mode::Bench
            | Mode::Peek
            | Mode::Rsh
            | Mode::Status
            | Mode::Watch
            | Mode::Log
            | Mode::Check
            | Mode::Out
            | Mode::Follow
            | Mode::Fetch
            | Mode::Kill
            | Mode::KillForce
            | Mode::Release
            | Mode::Gc
            | Mode::Close
            | Mode::Open => 1,
        };
        assert_eq!(Mode::ALL.into_iter().map(counted).sum::<usize>(), 17);
    }
}
