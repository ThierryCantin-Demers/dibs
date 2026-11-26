//! The machines the dibspatcher's own dibs reaches: its `machines.toml` and `machines.d`, read
//! for the cards each has.

use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Default)]
pub struct Inventory {
    /// Each machine's card aliases.
    cards: BTreeMap<String, Vec<String>>,
    /// The files read, as text, for a client that asks.
    pub text: String,
    /// Each file read, by its path beside `machines.toml`.
    pub files: BTreeMap<String, String>,
    /// The files there that could not be read, which is not the same as their being gone.
    pub unread: Vec<String>,
}

impl Inventory {
    /// Where the dibspatcher's dibs reads it: `DIBS_MACHINES`, else its config directory.
    pub fn path() -> PathBuf {
        if let Some(path) = std::env::var_os("DIBS_MACHINES").filter(|p| !p.is_empty()) {
            return PathBuf::from(path);
        }
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|c| !c.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            });
        config.join("dibs/machines.toml")
    }

    pub fn load() -> Inventory {
        let path = Inventory::path();
        let mut files = vec![("machines.toml".to_string(), path.clone())];
        let mut inventory = Inventory::default();
        if let Some(beside) = path.parent() {
            let dir = beside.join("machines.d");
            let mut extra: Vec<PathBuf> = Vec::new();
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => extra.push(entry.path()),
                            Err(e) => inventory.unread.push(format!("{}: {e}", dir.display())),
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => inventory.unread.push(format!("{}: {e}", dir.display())),
            }
            extra.retain(|p| p.extension().is_some_and(|e| e == "toml"));
            extra.sort();
            files.extend(extra.into_iter().filter_map(|p| {
                let name = p.file_name()?.to_str()?.to_string();
                Some((format!("machines.d/{name}"), p))
            }));
        }
        for (name, file) in files {
            match std::fs::read_to_string(&file) {
                Ok(text) => {
                    inventory.add(&text);
                    inventory.files.insert(name, text);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => inventory.unread.push(format!("{}: {e}", file.display())),
            }
        }
        inventory
    }

    #[cfg(test)]
    pub fn parse(text: &str) -> Inventory {
        let mut inventory = Inventory::default();
        inventory.add(text);
        inventory
    }

    /// One file's machines, each file a table of its own.
    fn add(&mut self, text: &str) {
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(text);
        let Ok(value) = text.parse::<toml::Table>() else {
            return;
        };
        let Some(machines) = value.get("machine").and_then(|m| m.as_table()) else {
            return;
        };
        for (name, entry) in machines {
            let aliases = entry
                .get("device")
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
                .filter_map(|d| d.get("alias").and_then(|a| a.as_str()))
                .map(String::from);
            self.cards.entry(name.clone()).or_default().extend(aliases);
        }
    }

    pub fn has_card(&self, machine: &str, alias: &str) -> bool {
        self.cards
            .get(machine)
            .is_some_and(|cards| cards.iter().any(|c| c == alias))
    }

    pub fn has(&self, machine: &str) -> bool {
        self.cards.contains_key(machine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_are_read_from_every_machine_entry() {
        let inventory = Inventory::parse(
            "[machine.a]\nssh = \"a\"\n\n  [[machine.a.device]]\n  kind = \"gpu\"\n  alias = \"gpu:one\"\n\n[machine.b]\nssh = \"b\"\n",
        );
        assert!(inventory.has_card("a", "gpu:one"));
        assert!(!inventory.has_card("b", "gpu:one"));
        assert!(inventory.has("b"));
    }
}
