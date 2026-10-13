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
    /// The command builds for GPU runtimes the card has none of.
    NoRuntime {
        alias: String,
        has: String,
        asked: Vec<&'static str>,
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
        Card::on(alias, entry)
    }

    /// The card `alias` names on the machine `entry` describes.
    pub fn on(alias: &Alias, entry: &Machine) -> Result<Card, CardError> {
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

    /// Refuses a command whose cargo features build for GPU runtimes the card has none of: the
    /// program falls back to another runtime without a word, measured as the one asked for.
    pub fn runs(&self, command: &str) -> Result<(), CardError> {
        let asked = runtimes_built_for(command);
        let has: Vec<&str> = self.runtimes.split(',').filter(|r| !r.is_empty()).collect();
        if asked.is_empty() || has.is_empty() || asked.iter().any(|r| has.contains(r)) {
            return Ok(());
        }
        Err(CardError::NoRuntime {
            alias: self.alias.clone(),
            has: has.join(", "),
            asked,
        })
    }
}

/// The GPU runtimes a cargo command enables by feature, a feature of a dependency included.
fn runtimes_built_for(command: &str) -> Vec<&'static str> {
    let words: Vec<&str> = command
        .split_whitespace()
        .map(|w| w.trim_matches(['\'', '"']))
        .collect();
    let lists = words.iter().enumerate().filter_map(|(at, word)| {
        word.strip_prefix("--features=").or_else(|| {
            matches!(*word, "--features" | "-F")
                .then(|| words.get(at + 1).copied())
                .flatten()
        })
    });
    let mut runtimes = Vec::new();
    for feature in lists.flat_map(|list| list.split(',')) {
        let runtime = match feature.rsplit('/').next().unwrap_or_default() {
            "cuda" => "cuda",
            "rocm" | "hip" => "rocm",
            "vulkan" => "vulkan",
            "metal" => "metal",
            _ => continue,
        };
        if !runtimes.contains(&runtime) {
            runtimes.push(runtime);
        }
    }
    runtimes
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
            CardError::NoRuntime { alias, has, asked } => {
                writeln!(
                    f,
                    "dibs: {alias} runs {has}, and this command builds for {}.",
                    asked.join(", ")
                )?;
                writeln!(
                    f,
                    "  Built for a runtime the card lacks, the program falls back to another without a word,"
                )?;
                writeln!(f, "  and its numbers pass for the runtime it asked for.")?;
                writeln!(
                    f,
                    "  Name a card that runs it (dibs --machines -v lists each card's runtimes)."
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_builds_for_the_runtimes_its_features_name() {
        assert_eq!(
            runtimes_built_for("cargo bench --features rocm --bench rows"),
            ["rocm"]
        );
        assert_eq!(
            runtimes_built_for("cd x && cargo test -F 'burn/cuda,fusion' && cargo test --features=hip"),
            ["cuda", "rocm"]
        );
        assert!(runtimes_built_for("cargo bench --bench rows -- rocm").is_empty());
    }

    #[test]
    fn a_card_refuses_only_a_command_built_for_none_of_its_runtimes() {
        let card = Card {
            alias: "gpu:rx5600".into(),
            runtimes: "vulkan".into(),
            ..Card::none()
        };
        assert!(card.runs("cargo bench --features rocm").is_err());
        assert!(card.runs("cargo bench --features rocm,vulkan").is_ok());
        assert!(card.runs("cargo bench").is_ok());
        let unknown = Card {
            runtimes: String::new(),
            ..card
        };
        assert!(unknown.runs("cargo bench --features rocm").is_ok());
    }
}
