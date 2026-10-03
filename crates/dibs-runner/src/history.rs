pub use dibs_format::status::Scope;
use dibs_format::{HistoryLine, Label, Mode};
use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::Path,
};

/// Lines kept once the history outgrows its bound.
const KEPT: usize = 500;
const BOUND: usize = 1000;

/// Every successful run's duration on this machine, which estimates are drawn from.
pub struct History {
    lines: Vec<HistoryLine>,
}

/// What a job's history says it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    /// The tenth percentile.
    pub low: u64,
    pub median: u64,
    /// The ninetieth percentile, by nearest rank, so never below the median.
    pub high: u64,
    pub runs: usize,
    pub scope: Scope,
    /// Drawn from the label's other procedures, since this one has not run.
    pub other: bool,
}

/// What a job is: the keys its estimate is looked up by.
#[derive(Debug, Clone, Copy)]
pub struct Key<'a> {
    pub mode: Mode,
    pub label: &'a Label,
    pub agent: Option<&'a str>,
    pub fingerprint: Option<&'a str>,
}

impl History {
    pub fn load(path: &Path) -> History {
        let lines = fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.parse().ok())
            .collect();
        History { lines }
    }

    /// The median and its spread, by the sharpest key that has any runs: the procedure, the
    /// label, the agent, then the mode. None when the mode has never run here.
    pub fn estimate(&self, key: Key) -> Option<Estimate> {
        let mode: Vec<&HistoryLine> = self.lines.iter().filter(|l| l.mode == key.mode).collect();
        let label: Vec<&HistoryLine> = mode
            .iter()
            .copied()
            .filter(|l| &l.label == key.label)
            .collect();
        let fingerprint = key.fingerprint.filter(|f| !f.is_empty());
        let procedure: Vec<&HistoryLine> = match fingerprint {
            Some(f) => label
                .iter()
                .copied()
                .filter(|l| l.fingerprint.as_deref() == Some(f))
                .collect(),
            None => Vec::new(),
        };
        let agent: Vec<&HistoryLine> = match key.agent.filter(|a| !a.is_empty()) {
            Some(a) => mode
                .iter()
                .copied()
                .filter(|l| l.agent.as_deref() == Some(a))
                .collect(),
            None => Vec::new(),
        };
        let (runs, scope, other) = if !procedure.is_empty() {
            (procedure, Scope::This, false)
        } else if !label.is_empty() {
            (label, Scope::This, fingerprint.is_some())
        } else if !agent.is_empty() {
            (agent, Scope::Agent, false)
        } else {
            (mode, Scope::Mode, false)
        };
        let mut seconds: Vec<u64> = runs.iter().map(|l| l.seconds).collect();
        seconds.sort_unstable();
        Estimate::of(&seconds, scope, other)
    }

    /// Records a run that did what it set out to, keeping the file to its bound.
    pub fn append(path: &Path, line: &HistoryLine) {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(format!("{line}\n").as_bytes());
        }
        Trim {
            path,
            bound: BOUND,
            kept: KEPT,
        }
        .run();
    }
}

impl Estimate {
    /// From durations sorted ascending, with the percentiles awk computed them by.
    fn of(sorted: &[u64], scope: Scope, other: bool) -> Option<Estimate> {
        let n = sorted.len();
        if n == 0 {
            return None;
        }
        let at = |rank: usize| sorted[rank.clamp(1, n) - 1];
        let median = match n % 2 {
            1 => at(n.div_ceil(2)),
            _ => (at(n / 2) + at(n / 2 + 1)) / 2,
        };
        let tail = 0.9 * n as f64;
        let mut high = tail as usize;
        if (high as f64) < tail {
            high += 1;
        }
        Some(Estimate {
            low: at((0.1 * n as f64) as usize),
            median,
            high: at(high),
            runs: n,
            scope,
            other,
        })
    }

    /// Seconds a job that has run `elapsed` still has, by the median and then by the tail.
    pub fn remaining(&self, elapsed: u64) -> Option<u64> {
        match elapsed {
            e if e < self.median => Some(self.median - e),
            e if e < self.high => Some(self.high - e),
            _ => None,
        }
    }
}

/// A file cut back to its last lines once it outgrows a bound.
pub struct Trim<'a> {
    pub path: &'a Path,
    pub bound: usize,
    pub kept: usize,
}

impl Trim<'_> {
    pub fn run(&self) {
        let Ok(text) = fs::read_to_string(self.path) else {
            return;
        };
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= self.bound {
            return;
        }
        let kept: String = lines[lines.len() - self.kept..]
            .iter()
            .map(|l| format!("{l}\n"))
            .collect();
        let temporary = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));
        if fs::write(&temporary, kept).is_ok() {
            let _ = fs::rename(&temporary, self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(lines: &str) -> History {
        History {
            lines: lines.lines().filter_map(|l| l.parse().ok()).collect(),
        }
    }

    fn key<'a>(label: &'a Label, fingerprint: Option<&'a str>) -> Key<'a> {
        Key {
            mode: Mode::Shared,
            label,
            agent: Some("me"),
            fingerprint,
        }
    }

    #[test]
    fn the_percentiles_are_awks() {
        let est = |seconds: &[u64]| Estimate::of(seconds, Scope::This, false).unwrap();
        let ten: Vec<u64> = (1..=10).collect();
        let e = est(&ten);
        assert_eq!((e.low, e.median, e.high, e.runs), (1, 5, 9, 10));
        let e = est(&[4, 9, 30]);
        assert_eq!((e.low, e.median, e.high), (4, 9, 30));
        let e = est(&[7]);
        assert_eq!((e.low, e.median, e.high), (7, 7, 7));
    }

    #[test]
    fn the_sharpest_key_with_runs_is_used() {
        let label = Label::new("build");
        let h = history(
            "shared\tbuild\t10\tme\tfp1\nshared\tbuild\t30\tme\tfp2\nshared\tother\t99\tme\t\nbench\tbuild\t500\tme\t\n",
        );
        let e = h.estimate(key(&label, Some("fp2"))).unwrap();
        assert_eq!((e.median, e.scope, e.other), (30, Scope::This, false));
        let e = h.estimate(key(&label, Some("fp9"))).unwrap();
        assert_eq!((e.median, e.scope, e.other), (20, Scope::This, true));
        let unseen = Label::new("unseen");
        let e = h.estimate(key(&unseen, None)).unwrap();
        assert_eq!((e.runs, e.scope), (3, Scope::Agent));
        assert!(history("").estimate(key(&label, None)).is_none());
    }
}
