//! What `dibs machines --json` says, and asking it again.

use serde::Deserialize;
use std::{
    collections::BTreeMap,
    process::{Command, Stdio},
};

#[derive(Deserialize, Clone, Default)]
pub struct Overview {
    pub people: Vec<String>,
    pub machines: Vec<Report>,
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

#[derive(Deserialize, Clone)]
pub struct Report {
    pub machine: String,
    pub provisioned: Provisioned,
    pub via: Via,
    pub unprobed: Option<String>,
    pub paths: Vec<PathCheck>,
    pub access: Access,
    pub findings: Vec<Finding>,
}

impl Report {
    /// The finding for one area, absent when the machine was not asked for it.
    pub fn finding(&self, area: &str) -> Option<&Finding> {
        self.findings.iter().find(|f| f.area == area)
    }
}

#[derive(Deserialize, Clone)]
pub struct Provisioned {
    pub by: Provisioner,
    pub source: Option<String>,
}

impl Provisioned {
    pub fn describe(&self) -> String {
        let by = match self.by {
            Provisioner::Ansible => "ansible",
            Provisioner::Hand => "hand",
        };
        match &self.source {
            Some(s) => format!("{by} ({s})"),
            None => by.to_string(),
        }
    }
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Provisioner {
    Ansible,
    Hand,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    Dibs,
    Ssh,
}

impl Via {
    pub fn describe(self) -> &'static str {
        match self {
            Via::Dibs => "through dibs, under its shared lock",
            Via::Ssh => "over ssh, not being in the pool yet",
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct PathCheck {
    pub name: String,
    pub problem: Option<String>,
}

#[derive(Deserialize, Clone, Default)]
pub struct Access {
    pub people: BTreeMap<String, Standing>,
    pub strangers: Vec<String>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Standing {
    Key,
    Missing,
    Unlisted,
    Tailnet,
}

impl Standing {
    pub fn describe(self) -> &'static str {
        match self {
            Standing::Key => "key",
            Standing::Missing => "missing",
            Standing::Unlisted => "unlisted",
            Standing::Tailnet => "tailnet",
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct Finding {
    pub area: String,
    pub ok: bool,
    pub detail: String,
}

/// Every machine, or the one named. The command exits 1 whenever anything is missing, which is
/// still a report, so its output is read whatever the status.
pub fn probe(only: Option<&str>) -> Result<Overview, String> {
    let mut cmd = Command::new("dibs");
    cmd.arg("machines")
        .args(only)
        .arg("--json")
        .stdin(Stdio::null());
    let out = cmd
        .output()
        .map_err(|e| format!("could not run dibs: {e}"))?;
    if out.stdout.iter().all(u8::is_ascii_whitespace) {
        let said = String::from_utf8_lossy(&out.stderr);
        let last = said
            .lines()
            .rfind(|l| l.starts_with("dibs: "))
            .unwrap_or("dibs: it printed no report");
        return Err(format!(
            "dibs machines: {}",
            last.trim_start_matches("dibs: ")
        ));
    }
    // A report in a shape this window does not know is an installed dibs older or newer than it.
    serde_json::from_slice(&out.stdout).map_err(|e| {
        format!("dibs machines printed a report this window cannot read ({e}); run dibs --update")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAID: &str = r#"{
      "people": ["alice", "bob"],
      "machines": [
        {"machine": "box", "provisioned": {"by": "ansible", "source": "box-ansible"}, "via": "dibs",
         "unprobed": null, "paths": [{"name": "box.local", "problem": null}],
         "access": {"people": {"alice": "key", "bob": "missing"}, "strangers": ["SHA256:x"]},
         "findings": [{"area": "login", "ok": false, "detail": "no key of bob"}]},
        {"machine": "mac", "provisioned": {"by": "hand", "source": null}, "via": "ssh",
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
                b.provisioned.describe(),
                b.access.people.get("bob").copied(),
                b.finding("login").map(|f| f.ok),
                b.finding("cuda").is_none()
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
