//! What a machine's CPUs, memory and cards are doing, for a status that asks. Nothing here forks:
//! a status is answered beside a benchmark, and a tool started there would be measured with it.

mod amd;
mod disk;
mod host;
mod nvml;

use crate::{lock::LockDir, machine::Site};
use dibs_format::status::{GpuMetrics, Metrics};
use std::{collections::BTreeMap, fs, path::Path};

/// Every reading there is, the CPU against the one the last caller left in the lock directory,
/// each card under the alias `aliases` gives its address.
pub fn read(machine: &Site, dir: &LockDir, aliases: &BTreeMap<String, String>) -> Metrics {
    let (cpu_busy_permille, cpu_over_ms) = host::Cpu::since_last(&dir.path.join("metrics.cpu"))
        .map_or((None, None), |busy| {
            (Some(busy.permille), Some(busy.over_ms))
        });
    let memory = host::Memory::read();
    let mut gpus = nvml::cards();
    gpus.extend(amd::cards());
    gpus.sort_by(|a, b| a.pci.cmp(&b.pci));
    named(&mut gpus, aliases);
    Metrics {
        cpu_busy_permille,
        cpu_over_ms,
        mem_used: memory.map(|m| m.used),
        mem_total: memory.map(|m| m.total),
        gpus,
        disks: disk::disks(machine),
    }
}

fn first_line(path: impl AsRef<Path>) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    Some(text.lines().next().unwrap_or_default().trim().to_string())
}

fn number(path: impl AsRef<Path>) -> Option<u64> {
    first_line(path)?.parse().ok()
}

fn named(gpus: &mut [GpuMetrics], aliases: &BTreeMap<String, String>) {
    for card in gpus {
        card.alias = aliases
            .iter()
            .find(|(pci, _)| pci.eq_ignore_ascii_case(&card.pci))
            .map(|(_, alias)| alias.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dibs_format::status::Vendor;

    #[test]
    fn a_card_is_named_by_the_alias_of_its_address_and_only_by_that() {
        let mut gpus = [
            GpuMetrics::of("0000:0a:00.0".into(), Vendor::Amd),
            GpuMetrics::of("0000:0b:00.0".into(), Vendor::Amd),
        ];
        let aliases = BTreeMap::from([("0000:0A:00.0".to_string(), "gpu:r9700.a".to_string())]);
        named(&mut gpus, &aliases);
        assert_eq!(
            (gpus[0].alias.as_deref(), gpus[1].alias.as_deref()),
            (Some("gpu:r9700.a"), None)
        );
    }
}
