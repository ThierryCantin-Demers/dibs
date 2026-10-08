//! `machines.toml`: the machines this computer knows, how to reach each, and the cards in it.
//! Every `machines.d/*.toml` beside it adds machines of its own, so a tool that leases machines
//! keeps a file of them without touching the person's.
//!
//! Read through serde and edited through `toml_edit`, so writing one machine's entry leaves the
//! rest of the file, comments included, as its owner wrote it.

use crate::paths::Paths;
use dibs_format::{Alias, MachineName, Moment};
use dibs_runner::shared::SharedFile;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table};

/// Every machine in the file, in the order the file lists them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inventory {
    /// Where checkouts live; `~/` is the home directory.
    pub root: Option<String>,
    /// A `default =` line, no longer read: a call names its machine.
    pub default: Option<String>,
    pub machines: Vec<Machine>,
    /// Machines whose lease has ended, kept apart so a call naming one is told so.
    pub ended: Vec<Machine>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Machine {
    #[serde(skip)]
    pub name: MachineName,
    /// The file the entry is in, which `dibs --check --write` writes it back to.
    #[serde(skip)]
    pub source: PathBuf,
    /// What ssh dials. A machine without one is listed and never reached.
    pub ssh: Option<String>,
    /// The ssh configuration to dial it with, in place of the user's own, for a machine reached
    /// through a bastion that only its own file describes; `~/` is the home directory.
    pub ssh_config: Option<String>,
    /// When it stops being a machine, in seconds since the epoch: a leased one ends with it.
    pub expires: Option<u64>,
    /// The name its runs are recorded under, shared by machines of one kind so a new one
    /// continues the series an earlier one began.
    pub series: Option<String>,
    /// The name the machine answers to, when the ssh string does not end in it.
    pub hostname: Option<String>,
    /// When `dibs --check --write` recorded it.
    pub probed: Option<String>,
    /// `measure = false` refuses a benchmark there.
    #[serde(default = "Machine::measures_unless_told")]
    pub measure: bool,
    /// Someone works at it, so placement prefers another.
    #[serde(default)]
    pub workstation: bool,
    #[serde(default, rename = "device")]
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Device {
    pub kind: Option<DeviceKind>,
    /// What `--device` names it by; a CPU has none.
    pub alias: Option<Alias>,
    pub name: Option<String>,
    pub cores: Option<u32>,
    /// The card's PCI address, `domain:bus:device.function`.
    pub pci: Option<String>,
    /// `vendor:device`, the ids a model shares with every card of that model.
    pub chip: Option<String>,
    /// The PCIe link it trained at, against what it supports.
    pub link: Option<String>,
    #[serde(default)]
    pub runtimes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Cpu,
    Gpu,
    #[serde(other)]
    Other,
}

#[derive(Debug)]
pub enum InventoryError {
    Read {
        path: PathBuf,
        error: io::Error,
    },
    Write {
        path: PathBuf,
        error: io::Error,
    },
    Parse {
        path: Option<PathBuf>,
        error: String,
    },
    NoSuchMachine(MachineName),
    /// A name a `machines.d` file gives a machine another file already has.
    Twice {
        name: MachineName,
        path: PathBuf,
    },
}

impl fmt::Display for InventoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InventoryError::Read { path, error } => write!(f, "{}: {error}", path.display()),
            InventoryError::Write { path, error } => {
                write!(f, "could not write {}: {error}", path.display())
            }
            InventoryError::Parse {
                path: Some(path),
                error,
            } => write!(f, "{}: {error}", path.display()),
            InventoryError::Parse { path: None, error } => f.write_str(error),
            InventoryError::NoSuchMachine(name) => write!(f, "no machine named '{name}'"),
            InventoryError::Twice { name, path } => write!(
                f,
                "{}: names the machine '{name}', which another inventory file already has",
                path.display()
            ),
        }
    }
}

impl std::error::Error for InventoryError {}

/// The file as serde reads it; the order of machines comes from the document.
#[derive(Deserialize)]
struct File {
    root: Option<String>,
    default: Option<String>,
    #[serde(default)]
    machine: BTreeMap<String, Machine>,
}

impl Inventory {
    /// This computer's, when there is a file; one that does not read is an error.
    pub fn here() -> Result<Option<Inventory>, InventoryError> {
        match Paths::from_env().inventory() {
            Some(path) => Inventory::load(&path),
            None => Ok(None),
        }
    }

    /// The machines this computer's inventory names, which are reached only through dibs.
    pub fn pool() -> Result<BTreeSet<String>, InventoryError> {
        Ok(Inventory::here()?
            .map(|i| i.names().map(MachineName::to_string).collect())
            .unwrap_or_default())
    }

    pub fn parse(text: &str) -> Result<Inventory, InventoryError> {
        let parse_error = |error: String| InventoryError::Parse { path: None, error };
        let file: File = toml::from_str(text).map_err(|e| parse_error(e.to_string()))?;
        let order = Inventory::document(text)?
            .get("machine")
            .and_then(|m| m.as_table_like())
            .map(|m| {
                m.iter()
                    .map(|(name, _)| name.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut by_name = file.machine;
        let machines = order
            .into_iter()
            .filter_map(|name| {
                let machine = by_name.remove(&name)?;
                Some(Machine {
                    name: MachineName::new(name),
                    ..machine
                })
            })
            .collect();
        Ok(Inventory {
            root: file.root,
            default: file.default,
            machines,
            ended: Vec::new(),
        })
    }

    /// None when there is no file there and no `machines.d` beside it.
    pub fn load(path: &Path) -> Result<Option<Inventory>, InventoryError> {
        let main = Inventory::read(path)?;
        let added = Inventory::added(path)?;
        if main.is_none() && added.is_empty() {
            return Ok(None);
        }
        let mut inventory = main.unwrap_or_default();
        for more in added {
            if let Some(twice) = more
                .machines
                .iter()
                .find(|m| inventory.machines.iter().any(|n| n.name == m.name))
            {
                return Err(InventoryError::Twice {
                    name: twice.name.clone(),
                    path: twice.source.clone(),
                });
            }
            inventory.machines.extend(more.machines);
        }
        let now = Moment::epoch_now();
        let (live, ended) = inventory
            .machines
            .into_iter()
            .partition(|m| m.expires.is_none_or(|at| at > now));
        inventory.machines = live;
        inventory.ended = ended;
        Ok(Some(inventory))
    }

    /// One file, each machine knowing it came from there; None when there is no file.
    fn read(path: &Path) -> Result<Option<Inventory>, InventoryError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(InventoryError::Read {
                    path: path.to_path_buf(),
                    error,
                });
            }
        };
        let mut inventory = Inventory::parse(&text).map_err(|e| e.at(path))?;
        for machine in &mut inventory.machines {
            machine.source = path.to_path_buf();
        }
        Ok(Some(inventory))
    }

    /// The files in `machines.d` beside `path`, in name order.
    fn added(path: &Path) -> Result<Vec<Inventory>, InventoryError> {
        let dir = path.with_file_name("machines.d");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(InventoryError::Read { path: dir, error }),
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        files.sort();
        files
            .iter()
            .filter_map(|file| Inventory::read(file).transpose())
            .collect()
    }

    /// The machine of that name whose lease has ended.
    pub fn ended(&self, name: &str) -> Option<&Machine> {
        self.ended.iter().find(|m| m.name.as_str() == name)
    }

    /// Every machine's name, reachable or not.
    pub fn names(&self) -> impl Iterator<Item = &MachineName> {
        self.machines.iter().map(|m| &m.name)
    }

    /// A machine a call can reach: one with an ssh string.
    pub fn machine(&self, name: &str) -> Option<&Machine> {
        self.reachable().find(|m| m.name.as_str() == name)
    }

    /// The machine an ssh string reaches, or whose hostname is the host after its `@`.
    pub fn by_host(&self, host: &str) -> Option<&Machine> {
        let bare = host.rsplit('@').next().unwrap_or(host);
        self.reachable()
            .find(|m| m.ssh.as_deref() == Some(host) || m.hostname.as_deref() == Some(bare))
    }

    /// `root`, with a leading `~/` read as the home directory.
    pub fn root(&self, home: Option<&Path>) -> Option<PathBuf> {
        let root = self.root.as_deref()?;
        Some(match (root.strip_prefix("~/"), home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(root),
        })
    }

    /// The file's text with `entry` as the machine's entry. Keys the old entry set and `entry`
    /// does not, such as a person's `measure` or a tool's `expires`, stay; its devices do not,
    /// since `entry` is a new reading of them.
    fn with_entry(text: &str, name: &MachineName, entry: &str) -> Result<String, InventoryError> {
        let old = Inventory::document(text)?;
        let mut new = Inventory::document(entry)?;
        if let (Some(old), Some(new)) = (
            old.get("machine")
                .and_then(|m| m.get(name.as_str()))
                .and_then(Item::as_table),
            new.get_mut("machine")
                .and_then(|m| m.get_mut(name.as_str()))
                .and_then(Item::as_table_mut),
        ) {
            for (key, item) in old.iter().filter(|(key, _)| *key != "device") {
                if !new.contains_key(key) {
                    new.insert(key, item.clone());
                }
            }
        }
        let without = Inventory::without(text, name)?;
        let written = format!("{without}\n{}\n", new.to_string().trim_end_matches('\n'));
        Inventory::parse(&written)?;
        Ok(written)
    }

    /// The file's text with the machine and its devices gone. Comments above its header stay,
    /// since they may be about the file rather than the machine.
    pub fn without(text: &str, name: &MachineName) -> Result<String, InventoryError> {
        let mut document = Inventory::document(text)?;
        let removed = document
            .get_mut("machine")
            .and_then(|m| m.as_table_mut())
            .and_then(|machines| machines.remove(name.as_str()));
        let above = removed.as_ref().and_then(|item| {
            let table = item.as_table()?;
            let prefix = table.decor().prefix()?.as_str()?;
            prefix
                .contains('#')
                .then(|| (table.position(), prefix.to_string()))
        });
        if let Some((position, prefix)) = above {
            Inventory::keep_above_next(&mut document, position.unwrap_or_default(), &prefix);
        }
        Ok(document.to_string())
    }

    /// Puts `prefix` above the first table after `position`, or at the end when none follows.
    fn keep_above_next(document: &mut DocumentMut, position: usize, prefix: &str) {
        let mut next = None;
        Inventory::each_table(document.as_table_mut(), &mut |table| {
            let at = table.position().filter(|p| *p > position);
            if at.is_some() && (next.is_none() || at < next) {
                next = at;
            }
        });
        let Some(next) = next else {
            let trailing = document.trailing().as_str().unwrap_or_default().to_string();
            document.set_trailing(format!("{trailing}{prefix}"));
            return;
        };
        Inventory::each_table(document.as_table_mut(), &mut |table| {
            if table.position() == Some(next) {
                let own = table.decor().prefix().and_then(|p| p.as_str());
                let own = own.unwrap_or_default().to_string();
                table.decor_mut().set_prefix(format!("{prefix}{own}"));
            }
        });
    }

    fn each_table(table: &mut Table, visit: &mut dyn FnMut(&mut Table)) {
        for (_, item) in table.iter_mut() {
            match item {
                Item::Table(inner) => {
                    visit(inner);
                    Inventory::each_table(inner, visit);
                }
                Item::ArrayOfTables(array) => {
                    for inner in array.iter_mut() {
                        visit(inner);
                        Inventory::each_table(inner, visit);
                    }
                }
                Item::None | Item::Value(_) => {}
            }
        }
    }

    /// Records the entry `dibs --check --write` printed, in place of any the machine had.
    pub fn write(path: &Path, name: &MachineName, entry: &str) -> Result<(), InventoryError> {
        Inventory::rewrite(path, |text| {
            Inventory::with_entry(text, name, entry).map_err(|e| e.at(path))
        })
    }

    /// Removes a machine and its devices; refused for one the file does not have.
    pub fn forget(path: &Path, name: &MachineName) -> Result<(), InventoryError> {
        Inventory::rewrite(path, |text| {
            let known = Inventory::parse(text)
                .map_err(|e| e.at(path))?
                .machine(name.as_str())
                .is_some();
            if !known {
                return Err(InventoryError::NoSuchMachine(name.clone()));
            }
            Inventory::without(text, name).map_err(|e| e.at(path))
        })
    }

    fn reachable(&self) -> impl Iterator<Item = &Machine> {
        self.machines
            .iter()
            .filter(|m| m.ssh.as_deref().is_some_and(|s| !s.is_empty()))
    }

    fn document(text: &str) -> Result<DocumentMut, InventoryError> {
        text.parse()
            .map_err(|e: toml_edit::TomlError| InventoryError::Parse {
                path: None,
                error: e.to_string(),
            })
    }

    /// Under the file's lock, and renamed over it whole, so a reader never sees half a file and
    /// two writers never lose each other's change.
    fn rewrite(
        path: &Path,
        change: impl FnOnce(&str) -> Result<String, InventoryError>,
    ) -> Result<(), InventoryError> {
        let mut refused = None;
        SharedFile { path }
            .rewrite(|text| match change(text) {
                Ok(written) => Some(written),
                Err(e) => {
                    refused = Some(e);
                    None
                }
            })
            .map_err(|error| InventoryError::Write {
                path: path.to_path_buf(),
                error,
            })?;
        refused.map_or(Ok(()), Err)
    }
}

impl InventoryError {
    fn at(self, path: &Path) -> InventoryError {
        match self {
            InventoryError::Parse { error, .. } => InventoryError::Parse {
                path: Some(path.to_path_buf()),
                error,
            },
            other => other,
        }
    }
}

impl Machine {
    fn measures_unless_told() -> bool {
        true
    }

    /// The bare name the machine answers to: its hostname, or the ssh string after its `@`.
    pub fn target(&self) -> Option<&str> {
        self.hostname.as_deref().or_else(|| {
            self.ssh
                .as_deref()
                .map(|s| s.rsplit('@').next().unwrap_or(s))
        })
    }

    /// `ssh_config` with a leading `~/` read as the home directory.
    pub fn ssh_config(&self, home: Option<&Path>) -> Option<PathBuf> {
        let file = self.ssh_config.as_deref()?;
        Some(match (file.strip_prefix("~/"), home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(file),
        })
    }

    pub fn device(&self, alias: &str) -> Option<&Device> {
        self.devices
            .iter()
            .find(|d| d.alias.as_ref().is_some_and(|a| a.as_str() == alias))
    }

    /// Every card's alias, in the order the entry lists them.
    pub fn aliases(&self) -> impl Iterator<Item = &Alias> {
        self.devices.iter().filter_map(|d| d.alias.as_ref())
    }

    /// The name of its CPU, when the entry lists one.
    pub fn cpu_name(&self) -> Option<&str> {
        self.devices
            .iter()
            .find(|d| d.kind == Some(DeviceKind::Cpu))
            .and_then(|d| d.name.as_deref())
    }

    /// How many of its cards share a chip id, which a selector keyed on the model cannot tell
    /// apart.
    pub fn chip_count(&self, chip: &str) -> usize {
        self.devices
            .iter()
            .filter(|d| d.alias.is_some() && d.chip.as_deref().unwrap_or_default() == chip)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARDS: &str = r#"default = "box-a"
root = "~/prog"

[machine.box-a]
ssh      = "dibs@box-a"
hostname = "box-a"
probed   = "2026-09-01"
workstation = true

  [[machine.box-a.device]]
  kind  = "cpu"
  name  = "a processor"
  cores = 16

  [[machine.box-a.device]]
  kind     = "gpu"
  alias    = "gpu:card"
  name     = "a card"
  pci      = "0000:01:00.0"
  chip     = "10de:2786"
  link     = "x16 of x16 at 16GT/s"
  runtimes = ["cuda", "vulkan"]

  [[machine.box-a.device]]
  kind     = "gpu"
  alias    = "gpu:other"
  name     = "another card"
  pci      = "0000:02:00.0"
  chip     = "10de:2786"
  runtimes = ["cuda", "vulkan"]

[machine.box-b]
ssh      = "box-b.local"
hostname = "box-b"
measure  = false
"#;

    const OLD_AND_NEW: &str = r#"[machine.old]
ssh      = "old"
hostname = "old"
measure  = false

  [[machine.old.device]]
  kind = "gpu"
  name = "a device whose parent is going away"

[machine.new]
ssh      = "new"
hostname = "new"
"#;

    fn names(inventory: &Inventory) -> Vec<&str> {
        inventory.names().map(MachineName::as_str).collect()
    }

    #[test]
    fn an_inventory_reads_its_machines_in_the_order_it_lists_them() {
        let inventory = Inventory::parse(CARDS).unwrap();
        assert_eq!(names(&inventory), ["box-a", "box-b"]);
        let unsorted =
            Inventory::parse("[machine.zed]\nssh = \"z\"\n\n[machine.alpha]\nssh = \"a\"\n")
                .unwrap();
        assert_eq!(names(&unsorted), ["zed", "alpha"]);
        assert_eq!(inventory.default.as_deref(), Some("box-a"));
        assert_eq!(
            inventory.root(Some(Path::new("/home/me"))),
            Some(PathBuf::from("/home/me/prog"))
        );
    }

    #[test]
    fn a_machine_says_how_to_reach_it_and_whether_it_measures() {
        let inventory = Inventory::parse(CARDS).unwrap();
        let a = inventory.machine("box-a").unwrap();
        assert_eq!(
            (a.ssh.as_deref(), a.target()),
            (Some("dibs@box-a"), Some("box-a"))
        );
        assert!(a.measure && a.workstation);
        let b = inventory.machine("box-b").unwrap();
        assert!(!b.measure && !b.workstation);
        assert_eq!(
            inventory.by_host("box-b.local").map(|m| m.name.as_str()),
            Some("box-b")
        );
        assert_eq!(
            inventory.by_host("me@box-a").map(|m| m.name.as_str()),
            Some("box-a")
        );
        assert_eq!(inventory.machine("box-z"), None);
    }

    #[test]
    fn a_machine_without_ssh_is_listed_and_never_reached() {
        let inventory = Inventory::parse(
            "[machine.here]\nssh = \"here\"\n\n[machine.half]\nhostname = \"half\"\n",
        )
        .unwrap();
        assert_eq!(names(&inventory), ["here", "half"]);
        assert_eq!(inventory.machine("half"), None);
    }

    #[test]
    fn a_card_is_found_by_its_alias_and_a_cpu_by_its_kind() {
        let inventory = Inventory::parse(CARDS).unwrap();
        let a = inventory.machine("box-a").unwrap();
        let aliases: Vec<&str> = a.aliases().map(Alias::as_str).collect();
        assert_eq!(aliases, ["gpu:card", "gpu:other"]);
        let card = a.device("gpu:card").unwrap();
        assert_eq!(card.pci.as_deref(), Some("0000:01:00.0"));
        assert_eq!(card.link.as_deref(), Some("x16 of x16 at 16GT/s"));
        assert_eq!(card.runtimes, ["cuda", "vulkan"]);
        assert_eq!(a.device("gpu:other").unwrap().link, None);
        assert_eq!(a.cpu_name(), Some("a processor"));
        assert_eq!(a.chip_count("10de:2786"), 2);
        assert_eq!(inventory.machine("box-b").unwrap().cpu_name(), None);
    }

    #[test]
    fn a_device_s_name_does_not_answer_for_its_machine() {
        let inventory = Inventory::parse(
            "[machine.desk]\nssh = \"dibs@desk\"\nhostname = \"desk\"\n\n  [[machine.desk.device]]\n  kind = \"gpu\"\n  name = \"a device, not the machine\"\n",
        )
        .unwrap();
        let desk = inventory.machine("desk").unwrap();
        assert_eq!(desk.hostname.as_deref(), Some("desk"));
        assert_eq!(desk.devices[0].kind, Some(DeviceKind::Gpu));
    }

    #[test]
    fn forgetting_a_machine_takes_its_devices_and_leaves_the_rest_as_written() {
        let commented = format!("# my machines\n{OLD_AND_NEW}");
        let text = Inventory::without(&commented, &MachineName::new("old")).unwrap();
        assert!(!text.contains("parent is going away"), "{text}");
        assert!(text.starts_with("# my machines\n"), "{text}");
        assert!(text.contains("[machine.new]\nssh      = \"new\""), "{text}");
        assert_eq!(names(&Inventory::parse(&text).unwrap()), ["new"]);
        let last = format!("{OLD_AND_NEW}# about the last one\n[machine.last]\nssh = \"l\"\n");
        let text = Inventory::without(&last, &MachineName::new("last")).unwrap();
        assert!(text.ends_with("# about the last one\n"), "{text}");
    }

    #[test]
    fn writing_an_entry_replaces_the_machine_s_and_keeps_its_own_comments() {
        let entry =
            "[machine.old]\nssh      = \"dibs@old\"\nmeasure  = false        # runs on a battery\n";
        let text = Inventory::with_entry(OLD_AND_NEW, &MachineName::new("old"), entry).unwrap();
        assert!(text.contains("# runs on a battery"), "{text}");
        assert!(!text.contains("parent is going away"), "{text}");
        let inventory = Inventory::parse(&text).unwrap();
        assert_eq!(names(&inventory), ["new", "old"]);
        let old = inventory.machine("old").unwrap();
        assert_eq!(
            (old.ssh.as_deref(), old.hostname.as_deref()),
            (Some("dibs@old"), Some("old"))
        );
    }

    #[test]
    fn an_entry_that_would_break_the_file_is_not_written() {
        assert!(Inventory::with_entry("", &MachineName::new("x"), "[machine.x\n").is_err());
    }

    #[test]
    fn write_and_forget_go_through_the_file() {
        let dir = std::env::temp_dir().join(format!("dibs-inventory-{}", std::process::id()));
        let path = dir.join("machines.toml");
        let name = MachineName::new("box");
        Inventory::write(&path, &name, "[machine.box]\nssh = \"dibs@box\"").unwrap();
        let read = Inventory::load(&path).unwrap().unwrap();
        assert_eq!(names(&read), ["box"]);
        Inventory::forget(&path, &name).unwrap();
        assert!(matches!(
            Inventory::forget(&path, &name),
            Err(InventoryError::NoSuchMachine(_))
        ));
        assert_eq!(Inventory::load(&dir.join("absent.toml")).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_machines_d_file_adds_machines_until_their_lease_ends() {
        let dir = std::env::temp_dir().join(format!("dibs-machines-d-{}", std::process::id()));
        let path = dir.join("machines.toml");
        std::fs::create_dir_all(dir.join("machines.d")).unwrap();
        let leased = dir.join("machines.d/leased.toml");
        let now = Moment::epoch_now();
        std::fs::write(
            &leased,
            format!(
                "[machine.live]\nssh = \"live\"\nssh_config = \"~/leased.ssh\"\nseries = \"kind\"\nexpires = {}\n\n\
                 [machine.gone]\nssh = \"gone\"\nexpires = {}\n",
                now + 3600,
                now - 1
            ),
        )
        .unwrap();
        std::fs::write(dir.join("machines.d/notes.txt"), "not an inventory").unwrap();
        let read = Inventory::load(&path).unwrap().unwrap();
        assert_eq!(names(&read), ["live"], "with no machines.toml at all");
        let live = read.machine("live").unwrap();
        assert_eq!(
            (
                live.source.as_path(),
                live.ssh_config(Some(Path::new("/h"))),
                live.series.as_deref()
            ),
            (
                leased.as_path(),
                Some(PathBuf::from("/h/leased.ssh")),
                Some("kind")
            )
        );
        assert!(read.ended("gone").is_some() && read.machine("gone").is_none());
        std::fs::write(&path, "[machine.live]\nssh = \"mine\"\n").unwrap();
        assert!(matches!(
            Inventory::load(&path),
            Err(InventoryError::Twice { .. })
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rewriting_an_entry_keeps_the_keys_the_new_one_does_not_set() {
        let text = "[machine.m]\nssh = \"m\"\nmeasure = false\nexpires = 9\n\n  [[machine.m.device]]\n  kind = \"cpu\"\n";
        let entry = "[machine.m]\nssh = \"m\"\nprobed = \"today\"\n";
        let written = Inventory::with_entry(text, &MachineName::new("m"), entry).unwrap();
        let m = Inventory::parse(&written).unwrap().machines.remove(0);
        assert_eq!(
            (m.measure, m.expires, m.probed.as_deref(), m.devices.len()),
            (false, Some(9), Some("today"), 0)
        );
    }
}
