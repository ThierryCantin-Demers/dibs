//! What a machine's runner says it has, which `dibs --check` and `dibs machines` judge against
//! what fleet.toml says it should have.

use crate::json::json_line;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What a machine has, as its runner reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub user: String,
    pub host: String,
    /// As `uname -s` says it: `Linux`, `Darwin`.
    pub os: String,
    pub arch: String,
    /// The bash every job runs under, by version.
    pub bash: Option<String>,
    /// rsync 3 or later, by version, which a tree sent from elsewhere arrives through.
    pub rsync: Option<String>,
    pub git: bool,
    pub cargo: bool,
    pub rustup: bool,
    /// Searched for a toolchain by its name or its name's prefix.
    pub toolchains: Vec<String>,
    /// The NVIDIA driver's version.
    pub nvidia: Option<String>,
    pub nvcc: Option<String>,
    pub vulkan: bool,
    /// The account may sudo without a password.
    pub nopasswd: bool,
    /// Read in order, for those that make the account root in all but name.
    pub groups: Vec<String>,
    /// Fingerprints of the keys in the account's `authorized_keys`, read in order and each
    /// asked whose it is.
    pub keys: Vec<String>,
    pub tailscale_ssh: bool,
    /// Each clone under `~/prog`, by name, with its origin; asked whether it holds a repo.
    pub repos: BTreeMap<String, String>,
}

impl Facts {
    pub fn to_line(&self) -> String {
        json_line(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_missing_a_field_do_not_read() {
        assert!(serde_json::from_str::<Facts>(r#"{"user":"u","host":"h"}"#).is_err());
    }

    #[test]
    fn facts_read_back_what_their_line_wrote() {
        let facts = Facts {
            user: "u".into(),
            toolchains: vec!["stable".into()],
            ..Facts::default()
        };
        let line = facts.to_line();
        assert_eq!(line.matches('\n').count(), 1);
        assert_eq!(serde_json::from_str::<Facts>(&line).unwrap(), facts);
    }
}
