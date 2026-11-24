//! `orchestrator.toml`: which pools build and which measure, and the build profile every split
//! is compiled with.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// The pool a split's build goes to.
    pub compile: String,
    /// The pool a split's measurement goes to.
    pub gpu: String,
    pub profile: Profile,
}

/// How every split is compiled, so a binary built on one machine runs on another: no native
/// CPU flags, one explicit baseline. A change of profile is a new baseline for every series.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    pub rustflags: String,
    /// `<ref>@sha256:<digest>`: the image every split builds in, whose glibc is the oldest any
    /// binary built here may need. None builds on the compile machine's own.
    pub image: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            compile: "compile".into(),
            gpu: "gpu".into(),
            profile: Profile::default(),
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: "linux-x86-64-v2".into(),
            rustflags: "-C target-cpu=x86-64-v2".into(),
            image: None,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Settings, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}
