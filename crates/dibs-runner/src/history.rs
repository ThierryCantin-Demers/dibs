use crate::shared::SharedFile;
pub use dibs_format::status::Scope;
use dibs_format::{HistoryLine, Label, Mode};
use std::{collections::HashMap, fs, path::Path};

/// Lines past which the history is compacted.
const BOUND: usize = 4000;
/// Runs a compaction keeps of each label: enough for its percentiles, however rarely it runs.
const PER_LABEL: usize = 50;
/// The most lines a compaction keeps, newest first, however many labels there are.
const KEPT: usize = 3000;
/// The newest runs an estimate is drawn from, so work that got faster or slower is predicted by
/// what it takes now rather than once most of its history has caught up.
const RECENT: usize = 10;

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

    /// The median and its spread by the sharpest key that has any runs: the newest of the
    /// procedure's or the label's, else all of the agent's, then the mode's. None when the mode
    /// has never run here.
    pub fn estimate(&self, key: Key) -> Option<Estimate> {
        self.drawn(key, RECENT)
    }

    /// The same over every run kept, for a bound that has to hold for the slowest of them.
    pub fn estimate_kept(&self, key: Key) -> Option<Estimate> {
        self.drawn(key, usize::MAX)
    }

    fn drawn(&self, key: Key, newest: usize) -> Option<Estimate> {
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
        // The other scopes mix labels, so their newest runs say nothing about this job's trend.
        let from = match scope {
            Scope::This => runs.len().saturating_sub(newest),
            Scope::Agent | Scope::Mode => 0,
        };
        let mut seconds: Vec<u64> = runs[from..].iter().map(|l| l.seconds).collect();
        seconds.sort_unstable();
        Estimate::of(&seconds, scope, other)
    }

    /// Records a run that did what it set out to, compacting the file past its bound.
    pub fn append(path: &Path, line: &HistoryLine) {
        let file = SharedFile { path };
        let _ = file.append(&line.to_string());
        let _ = file.rewrite(|text| (text.lines().count() > BOUND).then(|| compacted(text)));
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

/// The newest runs of each label, so a label run once a week keeps its estimate as long as one
/// run a hundred times a day; lines that do not read are dropped.
fn compacted(text: &str) -> String {
    let mut kept_of: HashMap<(Mode, Label), usize> = HashMap::new();
    let mut kept: Vec<&str> = text
        .lines()
        .rev()
        .filter(|line| {
            let Ok(run) = line.parse::<HistoryLine>() else {
                return false;
            };
            let count = kept_of.entry((run.mode, run.label)).or_default();
            *count += 1;
            *count <= PER_LABEL
        })
        .take(KEPT)
        .collect();
    kept.reverse();
    kept.iter().map(|line| format!("{line}\n")).collect()
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
    fn a_compaction_keeps_the_newest_runs_of_every_label() {
        let mut text = String::from("shared\tweekly\t30\tme\t\n");
        for n in 0..(PER_LABEL + 10) {
            text.push_str(&format!("shared\tbusy\t{n}\tme\t\nnot a line\n"));
        }
        let kept = compacted(&text);
        assert!(kept.starts_with("shared\tweekly\t30"));
        assert_eq!(kept.lines().count(), PER_LABEL + 1);
        assert!(kept.ends_with(&format!("shared\tbusy\t{}\tme\t\n", PER_LABEL + 9)));
        assert!(!kept.contains("not a line"));
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

    #[test]
    fn an_estimate_follows_the_newest_runs_and_a_bound_every_run_kept() {
        let label = Label::new("build");
        let mut lines = String::new();
        for _ in 0..20 {
            lines.push_str("shared\tbuild\t600\tme\t\n");
        }
        for _ in 0..RECENT {
            lines.push_str("shared\tbuild\t300\tme\t\n");
        }
        let h = history(&lines);
        let e = h.estimate(key(&label, None)).unwrap();
        assert_eq!((e.median, e.runs), (300, RECENT));
        let e = h.estimate_kept(key(&label, None)).unwrap();
        assert_eq!((e.median, e.high, e.runs), (600, 600, 20 + RECENT));
    }
}
