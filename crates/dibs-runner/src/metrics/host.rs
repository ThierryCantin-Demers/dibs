use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// A reading this soon after the last one is diffed against it but does not replace it, so two
/// callers polling together do not each leave the other an interval of nothing.
const SHORTEST_MS: u64 = 1000;

/// The CPU time spent busy over an interval.
pub struct Cpu {
    pub permille: u32,
    pub over_ms: u64,
}

/// `/proc/stat`'s first line: ticks of every CPU, and of those the idle and iowait ones.
#[derive(Clone, Copy)]
struct Ticks {
    total: u64,
    idle: u64,
}

impl Ticks {
    fn now() -> Option<Ticks> {
        let stat = fs::read_to_string("/proc/stat").ok()?;
        let fields: Vec<u64> = stat
            .lines()
            .next()?
            .strip_prefix("cpu ")?
            .split_whitespace()
            .map(|f| f.parse().unwrap_or(0))
            .collect();
        // Guest time is counted in user time already.
        let total = fields.iter().take(8).sum();
        let idle = fields.get(3)? + fields.get(4).copied().unwrap_or(0);
        Some(Ticks { total, idle })
    }
}

impl Cpu {
    /// Against the reading kept at `kept`, which this one replaces; None for the first.
    pub fn since_last(kept: &Path) -> Option<Cpu> {
        let now = Ticks::now()?;
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let last = fs::read_to_string(kept).ok().and_then(|text| {
            let mut fields = text.split_whitespace().map(|f| f.parse::<u64>().ok());
            Some((
                fields.next()??,
                Ticks {
                    total: fields.next()??,
                    idle: fields.next()??,
                },
            ))
        });
        let over_ms = last.map(|(at, _)| ms.saturating_sub(at));
        if over_ms.is_none_or(|over| over >= SHORTEST_MS) {
            let part = kept.with_file_name(format!(".metrics.cpu.{}", std::process::id()));
            if fs::write(&part, format!("{ms} {} {}\n", now.total, now.idle)).is_ok() {
                let _ = fs::rename(&part, kept);
            }
        }
        let (over_ms, (_, then)) = (over_ms?, last?);
        let total = now.total.checked_sub(then.total).filter(|t| *t > 0)?;
        let idle = now.idle.saturating_sub(then.idle).min(total);
        Some(Cpu {
            permille: ((total - idle) * 1000 / total) as u32,
            over_ms,
        })
    }
}

/// Bytes, from `/proc/meminfo`.
#[derive(Clone, Copy)]
pub struct Memory {
    pub used: u64,
    pub total: u64,
}

impl Memory {
    pub fn read() -> Option<Memory> {
        let info = fs::read_to_string("/proc/meminfo").ok()?;
        let kib = |key: &str| -> Option<u64> {
            info.lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        };
        let total = kib("MemTotal")? * 1024;
        let available = kib("MemAvailable")? * 1024;
        Some(Memory {
            used: total.saturating_sub(available),
            total,
        })
    }
}
