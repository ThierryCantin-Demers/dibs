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
    /// The cards after this one a call over several is pinned to, in the order named.
    pub also: Vec<Card>,
}

#[derive(Debug)]
pub enum CardError {
    /// The call's machine has no entry to name a card from.
    NoEntry { host: String },
    NoSuchCard {
        machine: MachineName,
        alias: Alias,
        aliases: Vec<Alias>,
        cpu: bool,
    },
    /// `--device` asks for more distinct cards than match it.
    TooFew { machine: MachineName, asked: Alias },
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
        Card::all_on(alias, entry)
    }

    /// `a,b`: each card named, the first carrying the rest, under the alias as written, which is
    /// what the series is filed under.
    pub fn all_on(alias: &Alias, entry: &Machine) -> Result<Card, CardError> {
        let alias = &Card::concrete(alias, entry, |_| 0)?;
        let mut cards = alias
            .as_str()
            .split(',')
            .map(|one| Card::on(&Alias::new(one), entry))
            .collect::<Result<Vec<Card>, CardError>>()?;
        let mut first = cards.remove(0);
        first.alias = alias.to_string();
        first.also = cards;
        Ok(first)
    }

    /// Every entry of `alias` made one card of `entry`, none taken twice. An alias named exactly
    /// stays, `gpu:` may be left out, and a prefix such as `aipror9700` takes the cheapest by
    /// `cost` of the cards whose alias it starts and no other entry names exactly, the first
    /// listed among equals.
    pub fn concrete(
        alias: &Alias,
        entry: &Machine,
        cost: impl Fn(&Alias) -> u64,
    ) -> Result<Alias, CardError> {
        let asked: Vec<&str> = alias.as_str().split(',').collect();
        let choices: Vec<Vec<Alias>> = asked.iter().map(|one| Card::matching(one, entry)).collect();
        let too_few = || CardError::TooFew {
            machine: entry.name.clone(),
            asked: alias.clone(),
        };
        let mut taken: Vec<Option<Alias>> = vec![None; asked.len()];
        for (at, one) in asked.iter().enumerate() {
            let card = match choices[at].as_slice() {
                [only] => only.clone(),
                [] => {
                    Card::on(&Alias::new(*one), entry)?;
                    Alias::new(*one)
                }
                _ => continue,
            };
            if taken.contains(&Some(card.clone())) {
                return Err(too_few());
            }
            taken[at] = Some(card);
        }
        let mut open: Vec<usize> = (0..asked.len()).filter(|&at| taken[at].is_none()).collect();
        open.sort_by_key(|&at| choices[at].len());
        let mut choices = choices;
        for list in &mut choices {
            list.sort_by_key(&cost);
        }
        let mut ceilings: Vec<u64> = choices.iter().flatten().map(&cost).collect();
        ceilings.sort_unstable();
        ceilings.dedup();
        let assigned = ceilings
            .iter()
            .chain(open.is_empty().then_some(&0))
            .find_map(|&ceiling| {
                let within: Vec<Vec<Alias>> = choices
                    .iter()
                    .map(|list| {
                        list.iter()
                            .filter(|a| cost(a) <= ceiling)
                            .cloned()
                            .collect()
                    })
                    .collect();
                let mut tried = taken.clone();
                Card::assign(&open, &within, &mut tried).then_some(tried)
            });
        match assigned {
            Some(assigned) => taken = assigned,
            None => return Err(too_few()),
        }
        let names: Vec<&str> = taken.iter().flatten().map(Alias::as_str).collect();
        Ok(Alias::new(names.join(",")))
    }

    /// Fills each of `open` with one of its `choices` no other entry has, the cheaper first, by
    /// augmenting paths: a greedy pass can spend a card a narrower entry needed, and a search
    /// over orderings takes factorial time to say there is no way.
    fn assign(open: &[usize], choices: &[Vec<Alias>], taken: &mut [Option<Alias>]) -> bool {
        fn place(
            at: usize,
            choices: &[Vec<Alias>],
            taken: &mut [Option<Alias>],
            fixed: &[bool],
            seen: &mut Vec<Alias>,
        ) -> bool {
            for card in &choices[at] {
                if seen.contains(card) {
                    continue;
                }
                seen.push(card.clone());
                let holder = taken.iter().position(|t| t.as_ref() == Some(card));
                let free = match holder {
                    None => true,
                    Some(other) => !fixed[other] && place(other, choices, taken, fixed, seen),
                };
                if free {
                    taken[at] = Some(card.clone());
                    return true;
                }
            }
            false
        }
        let fixed: Vec<bool> = taken.iter().map(Option::is_some).collect();
        open.iter()
            .all(|&at| place(at, choices, taken, &fixed, &mut Vec::new()))
    }

    /// `device` with `gpu:` before every entry that leaves it out, so that two spellings of one
    /// request are one label.
    pub fn spelled(device: &str) -> String {
        device
            .split(',')
            .map(|one| match one.contains(':') || one == "cpu" {
                true => one.to_string(),
                false => format!("gpu:{one}"),
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Whether two entries of `--device` could take the same card: one names it, or a model it is
    /// of, as the other does.
    pub fn may_meet(a: &str, b: &str) -> bool {
        let (a, b) = (Card::spelled(a), Card::spelled(b));
        let within = |x: &str, y: &str| x.strip_prefix(y).is_some_and(|rest| rest.starts_with('.'));
        a == b || within(&a, &b) || within(&b, &a)
    }

    /// Whether `alias` names more than one card for some entry, which a pick must then choose.
    pub fn chooses(alias: &Alias, entry: &Machine) -> bool {
        alias
            .as_str()
            .split(',')
            .any(|one| Card::matching(one, entry).len() > 1)
    }

    /// Whether each card of `cards` is one the same entry of `alias` names.
    pub fn fits(alias: &Alias, cards: &str, entry: &Machine) -> bool {
        let (asked, named): (Vec<&str>, Vec<&str>) = (
            alias.as_str().split(',').collect(),
            cards.split(',').collect(),
        );
        asked.len() == named.len()
            && asked.iter().zip(&named).all(|(one, card)| {
                Card::matching(one, entry)
                    .iter()
                    .any(|a| a.as_str() == *card)
            })
    }

    fn matching(one: &str, entry: &Machine) -> Vec<Alias> {
        if entry.device(one).is_some() {
            return vec![Alias::new(one)];
        }
        let named = match one.contains(':') {
            true => one.to_string(),
            false => format!("gpu:{one}"),
        };
        if entry.device(&named).is_some() {
            return vec![Alias::new(named)];
        }
        entry
            .aliases()
            .filter(|a| {
                a.as_str() == named
                    || a.as_str()
                        .strip_prefix(named.as_str())
                        .is_some_and(|rest| rest.starts_with('.'))
            })
            .cloned()
            .collect()
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
            also: Vec::new(),
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
        self.also.iter().try_for_each(|card| card.runs(command))?;
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
                aliases,
                cpu,
            } => {
                writeln!(f, "dibs: {machine} has no device called '{alias}'.")?;
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
            CardError::TooFew { machine, asked } => {
                writeln!(
                    f,
                    "dibs: --device {asked} needs a different card of {machine} for each entry, and fewer match."
                )?;
                writeln!(f, "  Its cards are listed by:  dibs --machines -v")
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
            runtimes_built_for(
                "cd x && cargo test -F 'burn/cuda,fusion' && cargo test --features=hip"
            ),
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

    fn machine(aliases: &[&str]) -> Machine {
        let devices: String = aliases
            .iter()
            .enumerate()
            .map(|(i, a)| {
                format!(
                    "[[machine.box.device]]\nkind = \"gpu\"\nalias = \"{a}\"\nname = \"x\"\npci = \"0000:{i:02x}:00.0\"\nchip = \"1:2\"\n"
                )
            })
            .collect();
        crate::inventory::Inventory::parse(&format!("[machine.box]\nssh = \"box\"\n{devices}"))
            .unwrap()
            .machine("box")
            .unwrap()
            .clone()
    }

    #[test]
    fn a_broad_prefix_leaves_a_narrower_one_the_cards_only_it_can_take() {
        let entry = machine(&["gpu:twin.41.00", "gpu:twin.41.01", "gpu:twin.83"]);
        let cards = Card::concrete(&Alias::new("twin,twin.41,twin.41"), &entry, |_| 0).unwrap();
        let mut cards: Vec<&str> = cards.as_str().split(',').collect();
        assert_eq!(cards.remove(0), "gpu:twin.83");
        cards.sort_unstable();
        assert_eq!(cards, ["gpu:twin.41.00", "gpu:twin.41.01"]);
    }

    #[test]
    fn an_exact_card_is_kept_from_a_prefix_and_never_named_twice() {
        let entry = machine(&["gpu:twin.a", "gpu:twin.b"]);
        let cards = Card::concrete(&Alias::new("twin,twin.a"), &entry, |_| 0).unwrap();
        assert_eq!(cards.as_str(), "gpu:twin.b,gpu:twin.a");
        assert!(Card::concrete(&Alias::new("gpu:twin.a,twin.a"), &entry, |_| 0).is_err());
    }

    #[test]
    fn a_free_card_is_taken_over_moving_an_entry_onto_a_busy_one() {
        let entry = machine(&["gpu:twin.41.a", "gpu:twin.41.b", "gpu:twin.83"]);
        let busy = |a: &Alias| u64::from(a.as_str() == "gpu:twin.41.b") * 1000;
        let cards = Card::concrete(&Alias::new("twin.41,twin"), &entry, busy).unwrap();
        assert_eq!(cards.as_str(), "gpu:twin.41.a,gpu:twin.83");
    }

    #[test]
    fn a_processor_with_no_alias_of_its_own_is_kept_as_named() {
        let entry = crate::inventory::Inventory::parse(
            "[machine.box]\nssh = \"box\"\n[[machine.box.device]]\nkind = \"cpu\"\nname = \"a processor\"\ncores = 4\n",
        )
        .unwrap()
        .machine("box")
        .unwrap()
        .clone();
        let cards = Card::concrete(&Alias::new("cpu"), &entry, |_| 0).unwrap();
        assert_eq!(cards.as_str(), "cpu");
    }

    #[test]
    fn a_model_no_card_count_can_satisfy_is_refused_at_once() {
        let names: Vec<String> = (0..12).map(|i| format!("gpu:twin.{i:02}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let entry = machine(&names);
        let asked = ["twin"; 13].join(",");
        assert!(Card::concrete(&Alias::new(asked), &entry, |_| 0).is_err());
        let asked = ["twin"; 12].join(",");
        assert!(Card::concrete(&Alias::new(asked), &entry, |_| 0).is_ok());
    }

    #[test]
    fn a_card_named_like_its_model_is_that_card() {
        let entry = machine(&["gpu:twin", "gpu:twin.a"]);
        let cards = Card::concrete(&Alias::new("twin"), &entry, |a| match a.as_str() {
            "gpu:twin" => 5,
            _ => 0,
        })
        .unwrap();
        assert_eq!(cards.as_str(), "gpu:twin");
    }

    #[test]
    fn two_spellings_of_a_card_may_meet_and_two_cards_do_not() {
        assert!(Card::may_meet("twin.a", "gpu:twin.a"));
        assert!(Card::may_meet("twin", "gpu:twin.a"));
        assert!(!Card::may_meet("gpu:twin.a", "gpu:twin.b"));
        assert!(!Card::may_meet("twin", "gpu:twins.a"));
    }
}
