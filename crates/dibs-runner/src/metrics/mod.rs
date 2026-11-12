//! What a machine's CPUs, memory and cards are doing, for a status that asks. Nothing here forks:
//! a status is answered beside a benchmark, and a tool started there would be measured with it.

mod amd;
mod host;
mod nvml;

use crate::lock::LockDir;
use dibs_format::status::Metrics;
use std::{fs, path::Path};

/// Every reading there is, the CPU against the one the last caller left in the lock directory.
pub fn read(dir: &LockDir) -> Metrics {
    let (cpu_busy_permille, cpu_over_ms) = host::Cpu::since_last(&dir.path.join("metrics.cpu"))
        .map_or((None, None), |busy| {
            (Some(busy.permille), Some(busy.over_ms))
        });
    let memory = host::Memory::read();
    let mut gpus = nvml::cards();
    gpus.extend(amd::cards());
    gpus.sort_by(|a, b| a.pci.cmp(&b.pci));
    Metrics {
        cpu_busy_permille,
        cpu_over_ms,
        mem_used: memory.map(|m| m.used),
        mem_total: memory.map(|m| m.total),
        gpus,
    }
}

fn first_line(path: impl AsRef<Path>) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    Some(text.lines().next().unwrap_or_default().trim().to_string())
}

fn number(path: impl AsRef<Path>) -> Option<u64> {
    first_line(path)?.parse().ok()
}
