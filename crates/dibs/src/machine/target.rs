use crate::{
    inventory::{Inventory, InventoryError, Machine},
    machine::ssh::Ssh,
};
use dibs_format::{Exit, MachineName, Moment, Span};
use std::{fmt, path::PathBuf};

/// The inventory a call resolves its machine against, and where it was looked for.
#[derive(Debug, Clone, Default)]
pub struct Fleet {
    pub path: Option<PathBuf>,
    pub inventory: Option<Inventory>,
}

/// What the environment says about where a call goes.
#[derive(Debug, Clone, Default)]
pub struct TargetEnv {
    /// `DIBS_HOST`: the one machine of a setup that has at most one.
    pub host: String,
    /// `DIBS_HOSTNAME`: the name that machine answers to.
    pub hostname: Option<String>,
    /// `DIBS_ON`.
    pub on: Option<String>,
    /// `DIBS_LOCAL=1`: every call runs on this computer.
    pub local: bool,
}

/// How a call came to its machine, which a diagnosis repeats back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    On,
    DibsOn,
    DibsHost,
    /// The inventory has one machine.
    Only,
    /// Ranked by load.
    Placed,
    Unnamed,
}

/// Where a call goes.
#[derive(Debug, Clone)]
pub struct Target {
    pub machine: Option<MachineName>,
    /// What ssh dials; empty when nothing names a machine.
    pub host: String,
    /// The bare name the machine answers to.
    pub hostname: String,
    pub measurable: bool,
    pub named: Named,
    /// `DIBS_HOST`, when an inventory of several machines means it no longer chooses one.
    pub unheeded: Option<String>,
    /// The ssh configuration the machine is dialled with, in place of the user's own.
    pub ssh_config: Option<PathBuf>,
    /// The entry's `series`: what it measures under in place of its own name.
    pub series: Option<String>,
}

#[derive(Debug)]
pub enum TargetError {
    /// A machine whose lease has ended, at that many seconds since the epoch.
    Ended {
        name: String,
        at: u64,
        file: PathBuf,
    },
    NoSuchMachine {
        name: String,
        inventory: Option<PathBuf>,
        known: Option<Vec<MachineName>>,
    },
    NoMachine {
        known: Vec<MachineName>,
        bench: bool,
        unheeded: Option<String>,
    },
    NotMeasured(MachineName),
}

impl Fleet {
    /// No inventory when there is no file; a file that does not read is an error, never absent.
    pub fn load(path: Option<PathBuf>) -> Result<Fleet, InventoryError> {
        let inventory = match path.as_deref() {
            Some(path) => Inventory::load(path)?,
            None => None,
        };
        Ok(Fleet { path, inventory })
    }

    pub fn names(&self) -> Vec<MachineName> {
        self.inventory
            .iter()
            .flat_map(|i| i.names().cloned())
            .collect()
    }

    /// Any entry by name, reachable or not, for what it says about its devices.
    pub fn entry(&self, name: &str) -> Option<&Machine> {
        self.inventory
            .as_ref()?
            .machines
            .iter()
            .find(|m| m.name.as_str() == name)
    }

    /// An entry a call can reach: one with an ssh string.
    pub fn reachable(&self, name: &str) -> Option<&Machine> {
        self.inventory.as_ref()?.machine(name)
    }

    /// An entry whose lease has ended.
    pub fn ended(&self, name: &str) -> Option<&Machine> {
        self.inventory.as_ref()?.ended(name)
    }

    /// Where the inventory is looked for, as messages name it.
    pub fn shown(&self) -> String {
        self.path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }

    /// Whether there is an inventory file, readable or not.
    pub fn exists(&self) -> bool {
        self.path.as_ref().is_some_and(|p| p.is_file())
    }
}

impl TargetEnv {
    pub fn from_env() -> TargetEnv {
        let set = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        TargetEnv {
            host: std::env::var("DIBS_HOST").unwrap_or_default(),
            hostname: set("DIBS_HOSTNAME"),
            on: set("DIBS_ON"),
            local: std::env::var("DIBS_LOCAL").is_ok_and(|v| v == "1"),
        }
    }
}

impl Target {
    /// A host taken as typed, which no entry names: how `--check` reaches a new machine.
    pub fn literal(host: &str) -> Target {
        Target {
            machine: None,
            host: host.to_string(),
            hostname: host.to_string(),
            measurable: true,
            named: Named::On,
            unheeded: None,
            ssh_config: None,
            series: None,
        }
    }

    pub fn resolve(
        on: Option<&MachineName>,
        env: &TargetEnv,
        fleet: &Fleet,
    ) -> Result<Target, TargetError> {
        let mut target = Target {
            machine: None,
            host: env.host.clone(),
            hostname: env
                .hostname
                .clone()
                .unwrap_or_else(|| Ssh::host_of(&env.host).to_string()),
            measurable: true,
            named: Named::Unnamed,
            unheeded: None,
            ssh_config: None,
            series: None,
        };
        let count = fleet.names().len();
        if let Some(on) = on {
            target.go_to(fleet, on.as_str(), Named::On)?;
        } else if let Some(on) = &env.on {
            target.go_to(fleet, on, Named::DibsOn)?;
        } else if !env.local && count > 1 {
            target.unheeded = Some(std::mem::take(&mut target.host)).filter(|h| !h.is_empty());
            target.hostname.clear();
        } else if !env.host.is_empty() && fleet.reachable(&env.host).is_some() {
            target.go_to(fleet, &env.host, Named::DibsHost)?;
        } else if let Some(name) = (!env.host.is_empty())
            .then(|| fleet.inventory.as_ref()?.by_host(&env.host))
            .flatten()
            .map(|m| m.name.clone())
        {
            target.go_to(fleet, name.as_str(), Named::DibsHost)?;
        } else if env.host.is_empty()
            && !env.local
            && count == 1
            && let Some(only) = fleet.names().first()
        {
            target.go_to(fleet, only.as_str(), Named::Only)?;
        }
        Ok(target)
    }

    pub fn go_to(&mut self, fleet: &Fleet, name: &str, named: Named) -> Result<(), TargetError> {
        let machine = fleet
            .reachable(name)
            .ok_or_else(|| match fleet.ended(name) {
                Some(ended) => TargetError::Ended {
                    name: name.to_string(),
                    at: ended.expires.unwrap_or_default(),
                    file: ended.source.clone(),
                },
                None => TargetError::NoSuchMachine {
                    name: name.to_string(),
                    inventory: fleet.path.clone(),
                    known: fleet.exists().then(|| fleet.names()),
                },
            })?;
        self.machine = Some(machine.name.clone());
        self.host = machine.ssh.clone().unwrap_or_default();
        self.hostname = machine.target().unwrap_or_default().to_string();
        self.measurable = machine.measure;
        self.named = named;
        self.ssh_config =
            machine.ssh_config(std::env::var_os("HOME").map(PathBuf::from).as_deref());
        self.series = machine.series.clone();
        Ok(())
    }

    /// What a series is keyed on: where the job goes, so one machine reached by two names is one
    /// series.
    pub fn series_key(&self) -> String {
        [
            self.series.clone().unwrap_or_default(),
            self.host.clone(),
            self.machine
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_default(),
            self.hostname.clone(),
        ]
        .into_iter()
        .find(|m| !m.is_empty())
        .unwrap_or_else(|| "?".into())
    }

    pub fn pinned(&self) -> bool {
        matches!(self.named, Named::On | Named::DibsOn)
    }

    /// The entry of the machine this call goes to, found by its ssh string or hostname when the
    /// call named none.
    pub fn entry<'a>(&self, fleet: &'a Fleet) -> Option<&'a Machine> {
        if let Some(name) = &self.machine {
            return fleet.entry(name.as_str());
        }
        fleet.inventory.as_ref()?.machines.iter().find(|m| {
            m.ssh.as_deref() == Some(self.host.as_str())
                || (!self.hostname.is_empty()
                    && m.hostname.as_deref() == Some(self.hostname.as_str()))
        })
    }

    pub fn no_machine(&self, fleet: &Fleet, bench: bool) -> TargetError {
        TargetError::NoMachine {
            known: fleet.names(),
            bench,
            unheeded: self.unheeded.clone(),
        }
    }
}

impl TargetError {
    pub fn exit(&self) -> Exit {
        match self {
            TargetError::Ended { .. } => Exit::Unreachable,
            _ => Exit::Refused,
        }
    }
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetError::Ended { name, at, file } => writeln!(
                f,
                "dibs: {name}'s lease ended {} ago, so it is gone; {} still lists it.",
                Span(Moment::epoch_now().saturating_sub(*at)),
                file.display()
            ),
            TargetError::NoSuchMachine {
                name,
                inventory,
                known,
            } => {
                let path = inventory
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                writeln!(f, "dibs: no machine named '{name}' in {path}")?;
                match known {
                    Some(known) => {
                        writeln!(f, "  known:")?;
                        for n in known {
                            writeln!(f, "    {n}")?;
                        }
                        Ok(())
                    }
                    None => writeln!(
                        f,
                        "  There is no inventory yet. Write one with:  dibs --check <host> --write"
                    ),
                }
            }
            TargetError::NoMachine {
                known,
                bench,
                unheeded,
            } => {
                if known.is_empty() {
                    writeln!(
                        f,
                        "dibs: no machine. Record one with:  dibs --check <host> --write"
                    )?;
                    return writeln!(
                        f,
                        "  Or, for a single machine and no inventory, set DIBS_HOST to it."
                    );
                }
                let names: Vec<&str> = known.iter().map(MachineName::as_str).collect();
                writeln!(
                    f,
                    "dibs: this call names no machine. Name one of: {}",
                    names.join(", ")
                )?;
                writeln!(
                    f,
                    "  with --on <machine>, or export DIBS_ON=<machine> to cover every call that follows."
                )?;
                if *bench {
                    writeln!(
                        f,
                        "  A measurement is never placed for you: its series belongs to the machine it ran on."
                    )?;
                }
                if let Some(host) = unheeded {
                    writeln!(
                        f,
                        "  DIBS_HOST={host} does not choose one when the inventory has several."
                    )?;
                }
                Ok(())
            }
            TargetError::NotMeasured(machine) => {
                writeln!(
                    f,
                    "dibs: {machine} is marked measure = false, so a benchmark cannot run there."
                )?;
                writeln!(
                    f,
                    "  Name one that measures with --on <machine>, or run it shared if it is not a measurement."
                )
            }
        }
    }
}
