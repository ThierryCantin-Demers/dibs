//! Which card each label's measurements were taken on, per machine. Two cards are two
//! histories, so a benchmark that moves to another card without saying so is refused.

use crate::machine::Fleet;
use dibs_format::{JobId, Moment};
use dibs_runner::shared::SharedFile;
use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

/// A file without this first line is from another keying and is ignored whole.
const HEADER: &str = "#dibs-series 1";

pub struct Series {
    pub path: PathBuf,
}

/// One label's series on one machine, as a run would file it.
pub struct Entry<'a> {
    pub label: &'a str,
    /// Where the job goes: its ssh string, inventory name or hostname.
    pub machine: &'a str,
    /// The alias the run named, or `none`.
    pub card: &'a str,
}

/// A benchmark that checked in, filed once it is followed to a success; filing it at the
/// check-in would move the series for a run that then fails.
pub struct Pending {
    pub label: String,
    pub machine: String,
    pub card: String,
    pub by: String,
    pub new_series: bool,
}

impl Pending {
    /// Where it waits: beside the job's kept log.
    pub fn path(kept_jobs: &Path, job: &JobId) -> PathBuf {
        kept_jobs.join(job.as_str()).join("series")
    }

    pub fn keep(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let new = if self.new_series { "new" } else { "same" };
        let _ = fs::write(
            path,
            format!(
                "{}\t{}\t{}\t{}\t{new}\n",
                self.label, self.machine, self.card, self.by
            ),
        );
    }

    /// The entry kept for this job, taken so it is filed once.
    pub fn take(path: &Path) -> Option<Pending> {
        let text = fs::read_to_string(path).ok()?;
        let _ = fs::remove_file(path);
        let mut fields = text.trim_end().split('\t').map(str::to_string);
        Some(Pending {
            label: fields.next()?,
            machine: fields.next()?,
            card: fields.next()?,
            by: fields.next()?,
            new_series: fields.next()? == "new",
        })
    }

    pub fn file(&self, series: &Series) {
        let entry = Entry {
            label: &self.label,
            machine: &self.machine,
            card: &self.card,
        };
        series.record(&entry, &self.by, self.new_series);
    }
}

#[derive(Debug)]
pub struct Moved {
    label: String,
    machine: String,
    before: String,
    by: String,
    now: String,
}

/// One line: label, machine, card, who, when and how many runs.
struct Line<'a> {
    fields: Vec<&'a str>,
}

impl<'a> Line<'a> {
    fn of(text: &'a str) -> Line<'a> {
        Line {
            fields: text.split('\t').collect(),
        }
    }

    fn field(&self, i: usize) -> &'a str {
        self.fields.get(i).copied().unwrap_or_default()
    }

    fn is(&self, label: &str, machine: &str) -> bool {
        self.field(0) == label && self.field(1) == machine
    }

    /// Its run count as awk reads a number: the leading digits, 0 without any.
    fn runs(&self) -> u64 {
        let digits: String = self
            .field(5)
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().unwrap_or(0)
    }
}

impl Series {
    /// The card `label`'s series on `machine` was measured on.
    pub fn card_of(&self, label: &str, machine: &str) -> Option<String> {
        let text = self.text()?;
        text.lines()
            .map(Line::of)
            .find(|l| l.is(label, machine))
            .map(|l| l.field(2).to_string())
    }

    fn text(&self) -> Option<String> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        (text.lines().next() == Some(HEADER)).then_some(text)
    }

    /// Ok with what to say on a first run here, or the refusal of a move to another card.
    pub fn check(
        &self,
        entry: &Entry,
        fleet: &Fleet,
        quiet_on_first: bool,
    ) -> Result<Option<String>, Moved> {
        let Some(text) = self.text() else {
            return Ok(None);
        };
        let Some(line) = text
            .lines()
            .map(Line::of)
            .find(|l| l.is(entry.label, entry.machine))
        else {
            if quiet_on_first {
                return Ok(None);
            }
            let elsewhere: Vec<String> = text
                .lines()
                .skip(1)
                .map(Line::of)
                .filter(|l| l.field(0) == entry.label && l.field(1) != entry.machine)
                .map(|l| {
                    let runs = l.field(5);
                    let counted = runs.parse::<f64>().is_ok_and(|n| n > 0.0);
                    match counted {
                        true => format!("{} ({runs} runs)", Series::name(fleet, l.field(1))),
                        false => Series::name(fleet, l.field(1)),
                    }
                })
                .collect();
            return Ok((!elsewhere.is_empty()).then(|| {
                format!(
                    "dibs: first run of '{}' on {}; its series is on {}.\n",
                    entry.label,
                    Series::name(fleet, entry.machine),
                    elsewhere.join(", ")
                )
            }));
        };
        match line.field(2) == entry.card {
            true => Ok(None),
            false => Err(Moved {
                label: entry.label.to_string(),
                machine: Series::name(fleet, entry.machine),
                before: line.field(2).to_string(),
                by: line.field(3).to_string(),
                now: entry.card.to_string(),
            }),
        }
    }

    /// Files a run that measured something, starting the series again when asked to, under the
    /// file's lock so two runs filed at once both count.
    pub fn record(&self, entry: &Entry, by: &str, new_series: bool) {
        let _ = SharedFile { path: &self.path }.rewrite(|text| {
            let text = match text.lines().next() == Some(HEADER) {
                true => text,
                false => "",
            };
            let runs = match new_series {
                false => text
                    .lines()
                    .map(Line::of)
                    .find(|l| l.is(entry.label, entry.machine))
                    .map(|l| l.runs())
                    .unwrap_or(0),
                true => 0,
            };
            let mut written = format!("{HEADER}\n");
            for line in text.lines().skip(1) {
                if !Line::of(line).is(entry.label, entry.machine) {
                    written.push_str(line);
                    written.push('\n');
                }
            }
            let when = Moment::epoch_now();
            written.push_str(&format!(
                "{}\t{}\t{}\t{by}\t{when}\t{}\n",
                entry.label,
                entry.machine,
                entry.card,
                runs + 1
            ));
            Some(written)
        });
    }

    /// Files what a batch filed on its machine: each line it changed from the copy it was sent
    /// replaces this file's line for the same label and machine.
    pub fn merge(&self, sent: &str, returned: &str) {
        if returned.lines().next() != Some(HEADER) {
            return;
        }
        let filed: Vec<&str> = returned
            .lines()
            .skip(1)
            .filter(|line| !sent.lines().skip(1).any(|was| was == *line))
            .collect();
        if filed.is_empty() {
            return;
        }
        let _ = SharedFile { path: &self.path }.rewrite(|text| {
            let text = match text.lines().next() == Some(HEADER) {
                true => text,
                false => "",
            };
            let mut written = format!("{HEADER}\n");
            for line in text.lines().skip(1) {
                let kept = Line::of(line);
                if !filed
                    .iter()
                    .map(|f| Line::of(f))
                    .any(|f| kept.is(f.field(0), f.field(1)))
                {
                    written.push_str(line);
                    written.push('\n');
                }
            }
            for line in &filed {
                written.push_str(line);
                written.push('\n');
            }
            Some(written)
        });
    }

    /// A machine as a person knows it: its inventory name, or the ssh string after its user.
    fn name(fleet: &Fleet, machine: &str) -> String {
        fleet
            .inventory
            .iter()
            .flat_map(|i| i.machines.iter())
            .find(|m| m.ssh.as_deref() == Some(machine))
            .map(|m| m.name.to_string())
            .unwrap_or_else(|| {
                machine
                    .split_once('@')
                    .map_or(machine, |(_, rest)| rest)
                    .to_string()
            })
    }

    /// A `--new-series` run that failed claims nothing, and says so, or the flag reads as ignored.
    pub fn stayed_put(label: &str) -> String {
        format!(
            "dibs: the command failed, so '{label}' did not start its series here again and is still filed as it was.\n  --new-series takes effect only when the run it is passed with succeeds. Pass it\n  again with a run that works, once, rather than on every run from here on.\n"
        )
    }
}

impl fmt::Display for Moved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "dibs: '{}' has been measured on another card of {}.",
            self.label, self.machine
        )?;
        match self.by.is_empty() {
            true => writeln!(f, "  before:  {}", self.before)?,
            false => writeln!(f, "  before:  {}, by {}", self.before, self.by)?,
        }
        writeln!(f, "  now:     {}", self.now)?;
        writeln!(
            f,
            "  Those are two histories, not one series, and a number from one cannot be"
        )?;
        writeln!(
            f,
            "  compared against a number from the other. Use the card it was measured on, or"
        )?;
        writeln!(
            f,
            "  start its series on this machine again deliberately:  --new-series"
        )
    }
}
