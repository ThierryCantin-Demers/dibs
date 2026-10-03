use dibs_format::status::{Idle, IdleKind};
use std::{fs, path::Path};

/// What one look at a holder's CPU found, measured against what the last look left in
/// `cpu.<pid>`: a cumulative count says only whether a job ever worked, so each look leaves its
/// count for the next to say whether it has moved.
pub struct Sample {
    /// Cores' worth, in hundredths, since the last look.
    pub rate: Option<u64>,
    pub idle: Option<Idle>,
}

/// When a job was last seen working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Worked {
    Never,
    At(u64),
    Unknown,
}

/// A look at a holder.
pub struct Look<'a> {
    pub file: &'a Path,
    /// Its tree's CPU, in clock ticks.
    pub ticks: u64,
    pub clock_ticks: u64,
    pub now: u64,
    /// When it took the lock.
    pub start: u64,
}

impl Look<'_> {
    pub fn sample(&self) -> Sample {
        let text = fs::read_to_string(self.file).unwrap_or_default();
        let mut words = text.split_whitespace();
        let number = |w: Option<&str>| -> Option<u64> {
            w.filter(|w| w.bytes().all(|b| b.is_ascii_digit()))?
                .parse()
                .ok()
        };
        let prev = number(words.next());
        let worked = match words.next() {
            Some("-") => Worked::Never,
            Some(w) => w.parse().map_or(Worked::Unknown, Worked::At),
            None => Worked::Unknown,
        };
        let at = number(words.next());
        let rate = match (prev, at) {
            (Some(prev), Some(at)) if self.now > at => {
                let worked = self.ticks as i64 - prev as i64;
                let over = (self.clock_ticks * (self.now - at)) as i64;
                u64::try_from(worked * 100 / over).ok()
            }
            _ => None,
        };
        let worked = match (self.ticks, worked) {
            (0, _) => Worked::Never,
            (_, Worked::Never | Worked::Unknown) => Worked::At(self.now),
            (ticks, _) if prev.is_none_or(|prev| ticks > prev) => Worked::At(self.now),
            (_, worked) => worked,
        };
        if self.now > at.unwrap_or(0) {
            let mark = match worked {
                Worked::At(t) => t.to_string(),
                Worked::Never | Worked::Unknown => "-".to_string(),
            };
            let _ = fs::write(self.file, format!("{} {mark} {}\n", self.ticks, self.now));
        }
        let idle = match worked {
            Worked::Never => Some(Idle {
                idle_for: self.now.saturating_sub(self.start),
                idle_kind: IdleKind::Never,
            }),
            Worked::At(t) if prev.is_some_and(|prev| self.ticks <= prev) => Some(Idle {
                idle_for: self.now.saturating_sub(t),
                idle_kind: IdleKind::Stalled,
            }),
            _ => None,
        };
        Sample { rate, idle }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(file: &Path, ticks: u64, now: u64) -> Sample {
        Look {
            file,
            ticks,
            clock_ticks: 100,
            now,
            start: 1000,
        }
        .sample()
    }

    #[test]
    fn a_tree_that_never_worked_is_idle_from_its_start_and_one_that_stops_from_its_last_work() {
        let dir = std::env::temp_dir().join(format!("dibs-cpu-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cpu.1");
        let first = look(&file, 0, 1010);
        assert_eq!(
            first.idle.map(|i| (i.idle_for, i.idle_kind)),
            Some((10, IdleKind::Never))
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "0 - 1010\n");
        let working = look(&file, 300, 1013);
        assert_eq!((working.rate, working.idle), (Some(100), None));
        let stalled = look(&file, 300, 1020);
        assert_eq!(
            stalled.idle.map(|i| (i.idle_for, i.idle_kind)),
            Some((7, IdleKind::Stalled))
        );
        assert_eq!(stalled.rate, Some(0));
        let _ = fs::remove_dir_all(&dir);
    }
}
