//! `--device` resolved against a machine's entry. A card is named rather than numbered, since an
//! index renumbers when a card is added, when the driver reorders, and between boots.

use crate::{
    inventory::{Device, Machine},
    machine::{Fleet, Target},
};
use dibs_format::{Alias, MachineName};
use std::fmt;

/// The card a call is pinned to, as the machine selects it.
#[derive(Debug, Clone, Default)]
pub struct Card {
    /// The alias `--device` named.
    pub alias: String,
    pub pci: String,
    /// Its runtimes, comma separated.
    pub runtimes: String,
    pub chip: String,
    /// How many cards there share its chip id.
    pub twins: usize,
}

#[derive(Debug)]
pub enum CardError {
    /// The call's machine has no entry to name a card from.
    NoEntry { host: String },
    NoSuchCard {
        machine: MachineName,
        alias: Alias,
        /// It has `gpu:<alias>`, which is not taken as this: two spellings would be two histories.
        gpu_spelling: bool,
        aliases: Vec<Alias>,
        cpu: bool,
    },
}

impl Card {
    /// No card named: the machine's runtime picks.
    pub fn none() -> Card {
        Card {
            twins: 1,
            ..Card::default()
        }
    }

    pub fn resolve(alias: &Alias, target: &Target, fleet: &Fleet) -> Result<Card, CardError> {
        let entry = target.entry(fleet).ok_or_else(|| CardError::NoEntry {
            host: target.host.clone(),
        })?;
        let device = entry.device(alias.as_str());
        let field =
            |f: fn(&Device) -> Option<&String>| device.and_then(f).cloned().unwrap_or_default();
        let pci = field(|d| d.pci.as_ref());
        let chip = field(|d| d.chip.as_ref());
        // A CPU needs no pinning and has no PCI address; a machine's only GPU, as on a Mac, has no
        // slot to be told apart by.
        let cpu = pci.is_empty() && alias.as_str() == "cpu" && entry.cpu_name().is_some();
        let sole = pci.is_empty() && !cpu && !field(|d| d.name.as_ref()).is_empty();
        if pci.is_empty() && !cpu && !sole {
            return Err(CardError::NoSuchCard {
                machine: entry.name.clone(),
                alias: alias.clone(),
                gpu_spelling: entry
                    .device(&format!("gpu:{alias}"))
                    .is_some_and(|d| d.pci.as_deref().is_some_and(|p| !p.is_empty())),
                aliases: entry.aliases().cloned().collect(),
                cpu: entry.cpu_name().is_some(),
            });
        }
        Ok(Card {
            alias: alias.to_string(),
            runtimes: device.map(|d| d.runtimes.join(",")).unwrap_or_default(),
            twins: entry.chip_count(&chip),
            pci,
            chip,
        })
    }

    /// A benchmark that names no card on a machine with several is not reproducible, and nothing
    /// downstream can tell its number from a pinned one.
    pub fn unpinned(entry: &Machine) -> Option<String> {
        let gpus = entry
            .aliases()
            .filter(|a| a.as_str().starts_with("gpu:"))
            .count();
        (gpus > 1).then(|| {
            format!(
                "dibs: {} has {gpus} GPUs and this benchmark named none of them.\n  It will run on whichever the runtime picks, which is not something you\n  can repeat on purpose, and the number will look like any other.\n  Name one with --device. The aliases are in:  dibs --machines -v\n",
                entry.name
            )
        })
    }
}

impl fmt::Display for CardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CardError::NoEntry { host } => {
                let (call, host) = match host.is_empty() {
                    true => ("this call", "<host>"),
                    false => (host.as_str(), host.as_str()),
                };
                writeln!(
                    f,
                    "dibs: --device names a card from a machine's entry, and {call} has none."
                )?;
                writeln!(f, "  Record one with:  dibs --check {host} --write")
            }
            CardError::NoSuchCard {
                machine,
                alias,
                gpu_spelling,
                aliases,
                cpu,
            } => {
                writeln!(f, "dibs: {machine} has no device called '{alias}'.")?;
                if *gpu_spelling {
                    writeln!(f, "  Did you mean gpu:{alias}?")?;
                }
                if aliases.is_empty() && !cpu {
                    return writeln!(
                        f,
                        "  Its entry lists no devices. Re-probe it:  dibs --check {machine} --write"
                    );
                }
                writeln!(f, "  it has:")?;
                for a in aliases {
                    writeln!(f, "    {a}")?;
                }
                if *cpu {
                    writeln!(f, "    cpu")?;
                }
                Ok(())
            }
        }
    }
}
