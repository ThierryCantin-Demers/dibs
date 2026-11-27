//! What `dibs machines` says of each machine against what fleet.toml says it should have: the
//! text it prints, `--json` and the machines window are all made from these.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every machine surveyed, and the people fleet.toml lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Overview {
    /// By name, as fleet.toml lists them; only written.
    pub people: Vec<String>,
    /// In the order they were surveyed, and read in it; looked up by machine name.
    pub machines: Vec<Report>,
}

/// One machine, against what it should have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub machine: String,
    pub provisioned: Provisioning,
    /// Why nothing was read from the machine, when nothing was.
    pub unprobed: Option<String>,
    /// In the order fleet.toml lists the names; only written.
    pub paths: Vec<PathCheck>,
    pub access: Access,
    /// In the order they were checked, and read in it; looked up by area.
    pub findings: Vec<Finding>,
}

/// How a machine was set up, as fleet.toml records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provisioning {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathCheck {
    pub name: String,
    pub problem: Option<String>,
}

/// Who can get in, as far as the machine can say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Access {
    /// Where each person stands, read by person.
    pub people: BTreeMap<String, Standing>,
    /// Fingerprints of keys that belong to nobody in fleet.toml, in the order the machine lists
    /// them; only written.
    pub strangers: Vec<String>,
}

/// Where one person stands on one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub area: Area,
    pub ok: bool,
    pub detail: String,
}

/// What a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

impl Overview {
    /// A probe of one machine replaces that machine's report and nothing else.
    ///
    /// # Guarantees
    ///
    /// - A report of a machine already here takes its place; one of a machine not here yet
    ///   is appended, in `fresh`'s order; every other report stays as it was.
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

    /// # Guarantees
    ///
    /// - True exactly when the machine was probed and every finding is fine, a probed machine
    ///   with no finding included.
    pub fn as_expected(&self) -> bool {
        self.unprobed.is_none() && self.findings.iter().all(|f| f.ok)
    }
}

impl Finding {
    /// Fine when nothing is missing, and then it says what is there; otherwise it says what is
    /// missing.
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

    /// # Guarantees
    ///
    /// - It is the name `--json` writes for the area.
    pub fn as_str(self) -> &'static str {
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
    /// # Guarantees
    ///
    /// - It is the name `--json` writes for the standing.
    pub fn as_str(self) -> &'static str {
        match self {
            Standing::Key => "key",
            Standing::Missing => "missing",
            Standing::Unlisted => "unlisted",
            Standing::Tailnet => "tailnet",
        }
    }
}

impl Provisioning {
    /// Who set it up, and from what where fleet.toml says: `ansible (box-ansible)`.
    pub fn to_text(&self) -> String {
        let by = match self.by {
            Provisioner::Ansible => "ansible",
            Provisioner::Hand => "hand",
        };
        match &self.source {
            Some(source) => format!("{by} ({source})"),
            None => by.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(machine: &str, findings: Vec<Finding>) -> Report {
        Report {
            machine: machine.into(),
            provisioned: Provisioning {
                by: Provisioner::Hand,
                source: None,
            },
            unprobed: None,
            paths: Vec::new(),
            access: Access::default(),
            findings,
        }
    }

    fn fine(area: Area) -> Finding {
        Finding::new(area, Vec::new(), "there".into())
    }

    fn overview(reports: Vec<Report>) -> Overview {
        Overview {
            people: Vec::new(),
            machines: reports,
        }
    }

    #[test]
    fn a_provisioning_names_its_source() {
        let by = Provisioning {
            by: Provisioner::Ansible,
            source: Some("box-ansible".into()),
        };
        assert_eq!(by.to_text(), "ansible (box-ansible)");
    }

    #[test]
    fn a_provisioning_with_no_source_names_only_who() {
        assert_eq!(report("box", Vec::new()).provisioned.to_text(), "hand");
    }

    #[test]
    fn a_report_finds_the_finding_for_an_area() {
        let r = report("box", vec![fine(Area::Login)]);
        assert_eq!(r.finding(Area::Login).map(|f| f.ok), Some(true));
    }

    #[test]
    fn a_report_has_no_finding_for_an_area_not_asked_for() {
        assert_eq!(
            report("box", vec![fine(Area::Login)]).finding(Area::Cuda),
            None
        );
    }

    #[test]
    fn nothing_missing_is_a_fine_finding_saying_what_is_there() {
        assert_eq!(
            Finding::new(Area::Rust, Vec::new(), "stable".into()),
            Finding {
                area: Area::Rust,
                ok: true,
                detail: "stable".into()
            }
        );
    }

    #[test]
    fn something_missing_is_a_finding_saying_all_of_it() {
        let missing = vec!["no rustup".into(), "no stable toolchain".into()];
        assert_eq!(
            Finding::new(Area::Rust, missing, "stable".into()),
            Finding {
                area: Area::Rust,
                ok: false,
                detail: "no rustup, no stable toolchain".into()
            }
        );
    }

    #[test]
    fn a_probed_machine_with_every_finding_fine_is_as_expected() {
        assert!(report("box", vec![fine(Area::Login)]).as_expected());
    }

    #[test]
    fn a_machine_missing_something_is_not_as_expected() {
        let missing = Finding::new(Area::Login, vec!["no key of bob".into()], String::new());
        assert!(!report("box", vec![fine(Area::Paths), missing]).as_expected());
    }

    #[test]
    fn a_machine_not_probed_is_not_as_expected() {
        let r = Report {
            unprobed: Some("unreachable: asleep".into()),
            ..report("mac", Vec::new())
        };
        assert!(!r.as_expected());
    }

    #[test]
    fn probing_one_machine_again_replaces_only_its_report() {
        let mut o = overview(vec![report("box", Vec::new()), report("mac", Vec::new())]);
        o.merge(overview(vec![report("box", vec![fine(Area::Login)])]));
        assert_eq!(
            o.machines
                .iter()
                .map(|r| (r.machine.as_str(), r.findings.len()))
                .collect::<Vec<_>>(),
            [("box", 1), ("mac", 0)]
        );
    }

    #[test]
    fn probing_a_machine_not_yet_surveyed_appends_its_report() {
        let mut o = overview(vec![report("box", Vec::new())]);
        o.merge(overview(vec![report("new", Vec::new())]));
        assert_eq!(
            o.machines
                .iter()
                .map(|r| r.machine.as_str())
                .collect::<Vec<_>>(),
            ["box", "new"]
        );
    }

    #[test]
    fn every_area_is_named_as_json_writes_it() {
        for area in Area::ALL {
            assert_eq!(serde_json::to_value(area).unwrap(), area.as_str());
        }
    }

    #[test]
    fn every_standing_is_named_as_json_writes_it() {
        let every = [
            Standing::Key,
            Standing::Missing,
            Standing::Unlisted,
            Standing::Tailnet,
        ];
        for standing in every {
            assert_eq!(serde_json::to_value(standing).unwrap(), standing.as_str());
        }
    }

    #[test]
    fn a_machine_is_looked_up_by_name() {
        let o = overview(vec![report("box", Vec::new()), report("mac", Vec::new())]);
        assert_eq!(o.machine("mac").map(|r| r.machine.as_str()), Some("mac"));
    }
}
