use crate::{
    machine::Machine,
    platform::{Host, Platform as _, Slot},
    settings::{home, var},
    stop::Signals,
};
use dibs_format::wire::{Card, Picked};
use std::{collections::BTreeMap, path::PathBuf, process::Command};

/// What a job finds in its environment besides what the runner inherited.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    vars: BTreeMap<&'static str, String>,
    /// `DIBS_PORT_<NAME>`, one per `--port`.
    ports: BTreeMap<String, String>,
}

/// A card the job cannot be pinned to, so it does not run: running it on whichever card is
/// first, under the name of the one asked for, would measure hardware nobody chose.
#[derive(Debug)]
pub struct Unpinned(pub String);

impl Environment {
    /// Nothing here has a person behind it, so a pager or a credential prompt is a hang; and
    /// scratch, not `/tmp`, which is a small tmpfs shared by everyone.
    pub fn of(machine: &Machine, card: Option<&Card>) -> Result<Environment, Unpinned> {
        let mut vars = BTreeMap::from([
            ("GIT_PAGER", "cat".to_string()),
            ("PAGER", "cat".into()),
            ("GIT_TERMINAL_PROMPT", "0".into()),
            ("DEBIAN_FRONTEND", "noninteractive".into()),
            ("DIBS_SCRATCH", machine.scratch.display().to_string()),
            ("TMPDIR", machine.tmp().display().to_string()),
            ("PATH", Environment::path()),
        ]);
        if let Some(card) = card
            && let Some(pci) = &card.pci
        {
            Pinned {
                card,
                pci,
                host: &machine.host,
            }
            .export(&mut vars)?;
        }
        Ok(Environment {
            vars,
            ports: BTreeMap::new(),
        })
    }

    /// A command over ssh runs in a shell that reads no profile, so a toolchain installed the
    /// ordinary way is not on its path.
    fn path() -> String {
        let mut path = var("PATH").unwrap_or_default();
        for dir in [
            home().join(".cargo/bin"),
            PathBuf::from("/usr/local/cuda/bin"),
        ] {
            let dir = dir.display().to_string();
            let present = path.split(':').any(|d| d == dir);
            if !present && PathBuf::from(&dir).is_dir() {
                path = format!("{dir}:{path}");
            }
        }
        path
    }

    pub fn set(&mut self, name: &'static str, value: String) {
        self.vars.insert(name, value);
    }

    pub fn port(&mut self, picked: &Picked) {
        self.ports.insert(
            format!("DIBS_PORT_{}", picked.name.to_ascii_uppercase()),
            picked.port.to_string(),
        );
    }

    pub fn apply(&self, command: &mut Command) {
        command.envs(&self.vars);
        command.envs(&self.ports);
    }
}

/// A job pinned to one card by its slot, which belongs to the slot rather than to the order the
/// driver enumerated in this boot.
struct Pinned<'a> {
    card: &'a Card,
    pci: &'a str,
    host: &'a str,
}

impl Pinned<'_> {
    fn export(&self, vars: &mut BTreeMap<&'static str, String>) -> Result<(), Unpinned> {
        let Pinned { card, pci, host } = self;
        vars.insert("DIBS_DEVICE", card.alias.to_string());
        vars.insert("DIBS_DEVICE_PCI", pci.to_string());
        vars.insert("CUDA_DEVICE_ORDER", "PCI_BUS_ID".into());
        let chip = card.chip.as_deref().map(str::to_ascii_lowercase);
        let holds = match Host::slot(pci) {
            Slot::Unreadable => None,
            Slot::Empty => Some(String::new()),
            Slot::Holds(chip) => Some(chip),
        };
        if let Some(holds) = holds
            && (holds.is_empty() || chip.as_ref().is_some_and(|c| *c != holds))
        {
            return Err(Unpinned(format!(
                "dibs: asked for {}, recorded as {} in {pci}, and that slot now holds {}.\n  \
                 Not running it on another card under that name.\n  \
                 Record what the machine holds now:  dibs --check {host} --write\n",
                card.alias,
                card.chip.as_deref().unwrap_or("a card"),
                match holds.is_empty() {
                    true => "nothing",
                    false => &holds,
                },
            )));
        }
        let runs = |runtime: &str| card.runtimes.iter().any(|r| r == runtime);
        if runs("cuda") {
            let uuid = Pinned::cuda_uuid(pci).ok_or_else(|| {
                Unpinned(format!(
                    "dibs: asked for {} ({pci}) and nothing here answers to it.\n  \
                     Not running it unpinned: that would measure whichever card is first\n  \
                     and report it under the name of the one you asked for.\n  \
                     Check the machine still has that card:  dibs --check {host} --write\n",
                    card.alias
                ))
            })?;
            vars.insert("CUDA_VISIBLE_DEVICES", uuid);
        }
        if runs("vulkan") {
            vars.insert("DRI_PRIME", format!("pci-{}", pci.replace([':', '.'], "_")));
            if let Some(chip) = &card.chip
                && card.twins == 1
            {
                vars.insert("MESA_VK_DEVICE_SELECT", chip.clone());
                vars.insert("MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE", "1".into());
            }
        }
        Ok(())
    }

    /// CUDA takes an index or a UUID, never a bus id, and ignores one without a word.
    fn cuda_uuid(pci: &str) -> Option<String> {
        let want = pci
            .split_once(':')
            .map_or(pci, |(_, bus)| bus)
            .to_ascii_lowercase();
        let mut query = Command::new("nvidia-smi");
        query.args(["--query-gpu=uuid,pci.bus_id", "--format=csv,noheader"]);
        Signals::unblocked(&mut query);
        let out = query.output().ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|line| {
                let (uuid, bus) = line.split_once(',')?;
                bus.trim()
                    .to_ascii_lowercase()
                    .ends_with(&want)
                    .then(|| uuid.trim().to_string())
            })
    }
}
