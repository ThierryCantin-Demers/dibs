use crate::{
    call::{base::CallError, machine::MachineCall},
    inventory::{Inventory, InventoryError, Machine},
};
use dibs_format::{Exit, MachineName};
use std::fmt;

/// Where a call goes before anything is placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Named(MachineName),
    /// Somewhere with no inventory name: a DIBS_HOST outside the inventory, or this computer.
    Unnamed,
    /// Nowhere until a machine is named, because several could take it.
    Unchosen,
}

impl MachineCall<'_> {
    /// `dibs --machines`: every machine the inventory names, with its cards under `-v`.
    pub fn machines(&self) -> Result<i32, CallError> {
        let Some(inventory) = self
            .fleet
            .inventory
            .as_ref()
            .filter(|_| self.fleet.exists())
        else {
            eprintln!("no inventory at {}", self.fleet.shown());
            eprintln!("Write one with:  dibs --check <host> --write");
            return Ok(i32::from(Exit::Refused.code()));
        };
        print!(
            "{}",
            Listing {
                machines: &inventory.machines,
                cards: self.call.verbose,
            }
        );
        let text = self
            .fleet
            .path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        if text.lines().any(defaults) {
            eprintln!(
                "  A 'default =' line in the inventory is no longer read: a call names its machine."
            );
        }
        Ok(0)
    }

    /// Where the call goes before anything is placed; refused when it names a machine the
    /// inventory cannot reach.
    pub fn destination(&self) -> Result<Destination, CallError> {
        let target = self.target()?;
        Ok(
            match (target.machine, target.host.is_empty() && !self.here.local) {
                (Some(machine), _) => Destination::Named(machine),
                (None, false) => Destination::Unnamed,
                (None, true) => Destination::Unchosen,
            },
        )
    }

    /// `dibs --which`: the machine this call would go to, by the name `--on` takes.
    pub fn which(&self) -> Result<i32, CallError> {
        let target = self.target()?;
        if let Some(machine) = &target.machine {
            println!("{machine}");
            return Ok(0);
        }
        let shown = self.fleet.shown();
        let known = self.fleet.names().len();
        if !target.host.is_empty() {
            eprintln!(
                "dibs: this call goes to '{}' from DIBS_HOST, which is not in the inventory at {shown}.",
                target.host
            );
            eprintln!(
                "  It has no name to give --on. Record it with:  dibs --check {} --write",
                target.host
            );
            return Ok(i32::from(Exit::Failed.code()));
        }
        if self.here.local {
            eprintln!("dibs: this call runs on this computer, under DIBS_LOCAL.");
            return Ok(i32::from(Exit::Failed.code()));
        }
        match known > 1 {
            true => eprintln!(
                "dibs: no machine: this call names none, and {shown} has {known} to choose from."
            ),
            false => eprintln!(
                "dibs: no machine: no --on, no DIBS_ON, no DIBS_HOST, and no inventory at {shown}."
            ),
        }
        Ok(i32::from(Exit::Refused.code()))
    }

    /// `dibs --forget <machine>`: its entry and its devices, gone from the inventory.
    pub fn forget(&self, name: &MachineName) -> Result<i32, CallError> {
        let Some(path) = self.fleet.path.as_deref().filter(|_| self.fleet.exists()) else {
            eprintln!("no inventory at {}", self.fleet.shown());
            return Ok(i32::from(Exit::Refused.code()));
        };
        match Inventory::forget(path, name) {
            Ok(()) => {
                println!("forgot {name}");
                Ok(0)
            }
            Err(InventoryError::NoSuchMachine(_)) => {
                eprintln!("dibs: no machine named '{name}'");
                Ok(i32::from(Exit::Refused.code()))
            }
            Err(e) => {
                eprintln!("dibs: {e}");
                Ok(i32::from(Exit::Failed.code()))
            }
        }
    }

    /// `dibs --measure <machine> on|off`, written in whichever file holds the machine.
    pub fn measure(&self, name: &MachineName, measures: bool) -> Result<i32, CallError> {
        let Some(path) = self.fleet.entry(name.as_str()).map(|m| m.source.clone()) else {
            eprintln!("dibs: no machine named '{name}'");
            return Ok(i32::from(Exit::Refused.code()));
        };
        match Inventory::set_measure(&path, name, measures) {
            Ok(()) => {
                match measures {
                    true => println!("{name} measures: a benchmark may run there."),
                    false => println!(
                        "{name} does not measure: a benchmark is refused there, and builds and tests still run."
                    ),
                }
                Ok(0)
            }
            Err(e) => {
                eprintln!("dibs: {e}");
                Ok(i32::from(Exit::Failed.code()))
            }
        }
    }
}

/// A line that would have chosen a machine for calls that name none.
fn defaults(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("default")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// The machines, one line each, and their cards under them when asked.
struct Listing<'a> {
    machines: &'a [Machine],
    cards: bool,
}

impl fmt::Display for Listing<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = self
            .machines
            .iter()
            .map(|m| m.name.as_str().len())
            .max()
            .unwrap_or_default();
        for machine in self.machines {
            let Some(ssh) = &machine.ssh else {
                writeln!(f, "   {:<width$} ", machine.name.as_str())?;
                continue;
            };
            let note = match machine.measure {
                true => "",
                false => "  (no measurements)",
            };
            writeln!(f, "   {:<width$} {ssh}{note}", machine.name.as_str())?;
            if !self.cards {
                continue;
            }
            for alias in machine.aliases() {
                let device = machine.device(alias.as_str());
                let field = |value: Option<&String>| value.cloned().unwrap_or_default();
                let link = match device.and_then(|d| d.link.as_ref()) {
                    Some(link) if !link.is_empty() => format!("  {link}"),
                    _ => String::new(),
                };
                writeln!(
                    f,
                    "     {:<28} {:<14} {}{link}",
                    alias.as_str(),
                    field(device.and_then(|d| d.pci.as_ref())),
                    device.map(|d| d.runtimes.join(", ")).unwrap_or_default(),
                )?;
            }
        }
        Ok(())
    }
}
