use crate::{
    call::{
        base::CallError,
        machine::{Asked, MachineCall},
    },
    inventory::Inventory,
    machine::{Delivery, Installed, Liveness, Provision, Session, Target, TargetEnv},
};
use dibs_format::{Exit, Label, MachineName, Mode};

/// The lines around the entry a machine prints for `--check --write`.
const ENTRY_START: &str = "--8<-- dibs inventory";
const ENTRY_END: &str = "--8<-- end";

impl MachineCall<'_> {
    /// `dibs --check [host]`: the runner installed there, what the machine has, and with
    /// `--write`, its entry recorded.
    pub fn check(&self, host: Option<&str>) -> Result<i32, CallError> {
        let target = match host {
            Some(host) if self.fleet.reachable(host).is_some() => Target::resolve(
                Some(&MachineName::new(host)),
                &TargetEnv::from_env(),
                &self.fleet,
            )?,
            Some(host) => Target::literal(host),
            None => self.target()?,
        };
        self.somewhere(&target)?;
        let provision = Provision {
            session: &Session::new(&target, &self.here),
            live: Liveness::from_env(),
        };
        let mut delivery = Delivery::Inherit;
        match provision.ensure(&mut delivery)? {
            Installed::Done | Installed::Unreached => {}
            Installed::NoneThere | Installed::Failed => return Ok(provision.failed(&mut delivery)),
        }
        if !self.call.write {
            return self.send(Asked::plain(Mode::Check, Label::new("check")), &target);
        }
        if target.machine.is_none() && target.hostname.is_empty() {
            eprintln!("dibs --check --write: name the machine, with --on or a host");
            return Ok(i32::from(Exit::Refused.code()));
        }
        let answer = self.capture(
            Asked::plain(Mode::Check, Label::new("check-write")),
            &target,
        )?;
        let status = answer.exit.unwrap_or_default();
        let said = String::from_utf8_lossy(&answer.output);
        let report = Report::read(said.trim_end_matches('\n'));
        print!("{}", report.shown);
        if report.entry.is_empty() {
            eprintln!("dibs: the machine reported nothing to record.");
            return Ok(status);
        }
        let Some(name) = target
            .machine
            .clone()
            .or_else(|| report.hostname().map(MachineName::new))
            .or_else(|| Some(MachineName::new(sanitized(&target.hostname))))
            .filter(|n| !n.as_str().is_empty())
        else {
            eprintln!("dibs: the machine reported no usable name; give it one with --on");
            return Ok(i32::from(Exit::Failed.code()));
        };
        let entry = report
            .entry
            .replace("@NAME@", name.as_str())
            .replace("@SSH@", &target.host);
        let written = self
            .fleet
            .path
            .as_deref()
            .is_some_and(|path| Inventory::write(path, &name, &entry).is_ok());
        if !written {
            eprintln!("dibs: could not write {}", self.fleet.shown());
            return Ok(i32::from(Exit::Failed.code()));
        }
        println!("  recorded as [machine.{name}] in {}", self.fleet.shown());
        Ok(status)
    }
}

/// What `--check --write` printed: the report to show, and the entry between its markers.
struct Report {
    shown: String,
    entry: String,
}

impl Report {
    fn read(said: &str) -> Report {
        let mut shown = String::new();
        let mut inside = Vec::new();
        let mut within = false;
        for line in said.split('\n') {
            if !within && line.starts_with(ENTRY_START) {
                within = true;
                continue;
            }
            match within {
                true if line.starts_with(ENTRY_END) => within = false,
                true => inside.push(line),
                false => {
                    shown.push_str(line);
                    shown.push('\n');
                }
            }
        }
        Report {
            shown,
            entry: inside.join("\n"),
        }
    }

    /// The machine's own short hostname, which names a machine nobody has used yet.
    fn hostname(&self) -> Option<String> {
        self.entry
            .lines()
            .find_map(|line| {
                let value = line.strip_prefix("hostname")?.trim_start_matches(' ');
                let value = value.strip_prefix('=')?.trim_start_matches(' ');
                let value = value.strip_prefix('"')?;
                Some(value[..value.rfind('"')?].to_string())
            })
            .map(|h| sanitized(&h))
            .filter(|h| !h.is_empty())
    }
}

/// A name with only the characters an inventory key keeps.
fn sanitized(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
        .collect()
}
