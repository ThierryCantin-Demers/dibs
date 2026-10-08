//! What `dibs machines` says of each machine against what fleet.toml says it should have. Its
//! JSON is what the machines window reads.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    pub people: Vec<String>,
    pub machines: Vec<Report>,
}

/// One machine, against what it should have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub machine: String,
    pub provisioned: Provisioned,
    /// Why nothing was read from the machine, when nothing was.
    pub unprobed: Option<String>,
    pub paths: Vec<PathCheck>,
    pub access: Access,
    pub findings: Vec<Finding>,
}

/// How a machine was set up, as fleet.toml records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provisioned {
    pub by: Provisioner,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provisioner {
    Ansible,
    Hand,
}

/// A name a machine is reached by, and why it does not reach it from here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathCheck {
    pub name: String,
    pub problem: Option<String>,
}

/// Who can get in, as far as the machine can say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    pub people: BTreeMap<String, Standing>,
    /// Fingerprints of keys that belong to nobody in fleet.toml.
    pub strangers: Vec<String>,
}

/// Where one person stands on one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Standing {
    /// Listed, with a key in `authorized_keys`.
    Key,
    /// Listed, with no key there.
    Missing,
    /// Not listed, with a key there anyway.
    Unlisted,
    /// Listed, on a machine the tailnet's policy decides.
    Tailnet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub area: Area,
    pub ok: bool,
    pub detail: String,
}

/// What a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Area {
    Paths,
    Login,
    Repos,
    Dibs,
    Rust,
    Cuda,
    Vulkan,
    Metal,
    Account,
}

/// What a machine has, as its runner reads it: what `dibs --check` and `dibs machines` judge.
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
    pub toolchains: Vec<String>,
    /// The NVIDIA driver's version.
    pub nvidia: Option<String>,
    pub nvcc: Option<String>,
    pub vulkan: bool,
    /// The account may sudo without a password.
    pub nopasswd: bool,
    pub groups: Vec<String>,
    /// Fingerprints of the keys in the account's `authorized_keys`.
    pub keys: Vec<String>,
    pub tailscale_ssh: bool,
    /// Each clone under `~/prog`, by name, with its origin.
    pub repos: BTreeMap<String, String>,
}

impl Facts {
    /// The document as one line of JSON, as the runner prints it.
    pub fn line(&self) -> String {
        format!("{}\n", serde_json::to_string(self).unwrap_or_default())
    }
}

impl Overview {
    /// A probe of one machine replaces that machine's report and nothing else.
    pub fn merge(&mut self, fresh: Overview) {
        for r in fresh.machines {
            match self.machines.iter_mut().find(|m| m.machine == r.machine) {
                Some(old) => *old = r,
                None => self.machines.push(r),
            }
        }
    }

    pub fn machine(&self, name: &str) -> Option<&Report> {
        self.machines.iter().find(|m| m.machine == name)
    }
}

impl Report {
    /// The finding for one area, absent when the machine was not asked for it.
    pub fn finding(&self, area: Area) -> Option<&Finding> {
        self.findings.iter().find(|f| f.area == area)
    }

    pub fn as_expected(&self) -> bool {
        self.unprobed.is_none() && self.findings.iter().all(|f| f.ok)
    }
}

impl Finding {
    /// Fine when nothing is missing, and then it says what is there.
    pub fn new(area: Area, missing: Vec<String>, present: String) -> Finding {
        match missing.is_empty() {
            true => Finding {
                area,
                ok: true,
                detail: present,
            },
            false => Finding {
                area,
                ok: false,
                detail: missing.join(", "),
            },
        }
    }
}

impl Area {
    pub const ALL: [Area; 9] = [
        Area::Paths,
        Area::Login,
        Area::Repos,
        Area::Dibs,
        Area::Rust,
        Area::Cuda,
        Area::Vulkan,
        Area::Metal,
        Area::Account,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Area::Paths => "paths",
            Area::Login => "login",
            Area::Repos => "repos",
            Area::Dibs => "dibs",
            Area::Rust => "rust",
            Area::Cuda => "cuda",
            Area::Vulkan => "vulkan",
            Area::Metal => "metal",
            Area::Account => "account",
        }
    }
}

impl Standing {
    pub fn name(self) -> &'static str {
        match self {
            Standing::Key => "key",
            Standing::Missing => "missing",
            Standing::Unlisted => "unlisted",
            Standing::Tailnet => "tailnet",
        }
    }
}

impl fmt::Display for Provisioned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let by = match self.by {
            Provisioner::Ansible => "ansible",
            Provisioner::Hand => "hand",
        };
        match &self.source {
            Some(s) => write!(f, "{by} ({s})"),
            None => f.write_str(by),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAID: &str = r#"{
      "people": ["alice", "bob"],
      "machines": [
        {"machine": "box", "provisioned": {"by": "ansible", "source": "box-ansible"},
         "unprobed": null, "paths": [{"name": "box.local", "problem": null}],
         "access": {"people": {"alice": "key", "bob": "missing"}, "strangers": ["SHA256:x"]},
         "findings": [{"area": "login", "ok": false, "detail": "no key of bob"}]},
        {"machine": "mac", "provisioned": {"by": "hand", "source": null},
         "unprobed": "unreachable: asleep", "paths": [], "access": {"people": {}, "strangers": []},
         "findings": []}
      ]
    }"#;

    #[test]
    fn the_report_dibs_prints_is_read_whole() {
        let o: Overview = serde_json::from_str(SAID).unwrap();
        let b = o.machine("box").unwrap();
        assert_eq!(
            (
                b.provisioned.to_string(),
                b.access.people.get("bob").copied(),
                b.finding(Area::Login).map(|f| f.ok),
                b.finding(Area::Cuda).is_none()
            ),
            (
                "ansible (box-ansible)".to_string(),
                Some(Standing::Missing),
                Some(false),
                true
            )
        );
        assert_eq!(
            o.machine("mac").unwrap().unprobed.as_deref(),
            Some("unreachable: asleep")
        );
    }

    #[test]
    fn probing_one_machine_again_replaces_only_its_report() {
        let mut o: Overview = serde_json::from_str(SAID).unwrap();
        let mut fresh: Overview = serde_json::from_str(SAID).unwrap();
        fresh.machines.retain(|m| m.machine == "mac");
        fresh.machines[0].unprobed = None;
        o.merge(fresh);
        assert_eq!(
            (
                o.machines.len(),
                o.machine("mac").unwrap().unprobed.is_none(),
                o.machine("box").unwrap().paths.len()
            ),
            (2, true, 1)
        );
    }
}
