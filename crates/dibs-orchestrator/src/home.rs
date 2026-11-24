//! Where the orchestrator keeps everything, under the account it runs as. `DIBS_ORCH_HOME` puts
//! it all under one directory instead, for a test.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Home {
    state: PathBuf,
    config: PathBuf,
}

impl Home {
    pub fn from_env() -> Home {
        if let Some(root) = std::env::var_os("DIBS_ORCH_HOME").filter(|r| !r.is_empty()) {
            let root = PathBuf::from(root);
            return Home {
                state: root.join("state"),
                config: root.join("config"),
            };
        }
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let xdg = |var: &str, under: &str| {
            std::env::var_os(var)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(under))
        };
        Home {
            state: xdg("XDG_STATE_HOME", ".local/state").join("dibs-orchestrator"),
            config: xdg("XDG_CONFIG_HOME", ".config").join("dibs-orchestrator"),
        }
    }

    pub fn store(&self) -> PathBuf {
        self.state.join("store.db")
    }

    /// A coworker's uploads, each under `stage/<key>`, and the copy the next is sent against.
    pub fn staging(&self, coworker: &str) -> PathBuf {
        self.state.join("staging").join(coworker)
    }

    pub fn submission(&self, id: &str) -> PathBuf {
        self.state.join("subs").join(id)
    }

    /// Bundles relayed from a compile machine, by digest.
    pub fn bundle(&self, digest: &str) -> PathBuf {
        self.state.join("cas/bundles").join(digest)
    }

    /// Held while a coworker's repo builds, since its tree on the compile machine is keyed by
    /// coworker and repo and two submissions would send their trees over each other's build.
    pub fn tree_lock(&self, coworker: &str, repo: &str) -> PathBuf {
        self.state
            .join("locks")
            .join(format!("{coworker}--{repo}.lock"))
    }

    pub fn splits(&self, repo: &str) -> PathBuf {
        self.config.join("splits").join(format!("{repo}.toml"))
    }

    pub fn settings(&self) -> PathBuf {
        self.config.join("orchestrator.toml")
    }
}

/// Text that names one path component: what a coworker, a key, a repo or an id may be.
pub fn component(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && !text.starts_with('.')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
