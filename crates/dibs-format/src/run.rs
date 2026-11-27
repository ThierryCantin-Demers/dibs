use crate::{Alias, BatchId, JobId, Lock, MachineName, wire::Revision};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{MapAccess, Visitor},
    ser::SerializeMap,
};
use serde_json::ser::{CharEscape, Formatter};
use std::{collections::BTreeMap, fmt, io, str::FromStr};

/// What a run record says was run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunVerb {
    Build,
    Test,
    Bench,
    /// A one-off command in a prepared tree, borrowing a build's machinery.
    Shell,
    /// A command with nothing prepared for it.
    Raw,
    /// A command run against a repo's servers.
    With,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Failed,
}

impl Outcome {
    /// How a run of these steps ended.
    ///
    /// # Guarantees
    ///
    /// - Ok when every step exited 0, a run of no steps included.
    pub fn of_steps(steps: &[StepRecord]) -> Outcome {
        match steps.iter().all(|s| s.status == 0) {
            true => Outcome::Ok,
            false => Outcome::Failed,
        }
    }
}

/// One named value, in the order it was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub name: String,
    pub value: String,
}

/// Named values written as a JSON object in the order they were recorded.
///
/// # Guarantees
///
/// - Iterates, writes and reads its values in the order they were recorded, a repeated name
///   each time it was.
/// - `sorted` keeps a repeated name's last value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pairs(
    /// Read in the order recorded.
    pub Vec<Pair>,
);

impl Pairs {
    /// Whether no value was recorded.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The values, in the order they were recorded.
    pub fn iter(&self) -> impl Iterator<Item = &Pair> {
        self.0.iter()
    }

    /// By name, the way a reader that did not keep the order saw them.
    pub fn sorted(&self) -> BTreeMap<&str, &str> {
        self.iter()
            .map(|p| (p.name.as_str(), p.value.as_str()))
            .collect()
    }
}

impl From<Vec<(String, String)>> for Pairs {
    fn from(pairs: Vec<(String, String)>) -> Self {
        Pairs(
            pairs
                .into_iter()
                .map(|(name, value)| Pair { name, value })
                .collect(),
        )
    }
}

impl From<Vec<Revision>> for Pairs {
    fn from(revisions: Vec<Revision>) -> Self {
        revisions
            .into_iter()
            .map(|r| Pair {
                name: r.repo,
                value: r.sha,
            })
            .collect()
    }
}

impl FromIterator<Pair> for Pairs {
    fn from_iter<I: IntoIterator<Item = Pair>>(pairs: I) -> Self {
        Pairs(pairs.into_iter().collect())
    }
}

impl Serialize for Pairs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for pair in &self.0 {
            map.serialize_entry(&pair.name, &pair.value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Pairs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(PairsVisitor)
    }
}

struct PairsVisitor;

impl<'de> Visitor<'de> for PairsVisitor {
    type Value = Pairs;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object of strings")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Pairs, A::Error> {
        let mut pairs = Vec::new();
        while let Some((name, value)) = map.next_entry()? {
            pairs.push(Pair { name, value });
        }
        Ok(Pairs(pairs))
    }
}

/// A step of the recipe as it was written, kept so a local recipe stays recoverable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureStep {
    pub lock: Lock,
    pub run: String,
}

/// One job a run started, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    pub lock: Lock,
    pub status: i32,
    pub seconds: u64,
    /// Which arm of a comparison, and which rep, the step belonged to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rep: Option<u32>,
    /// How many files it kept for the caller.
    #[serde(rename = "artifacts", default, skip_serializing_if = "Option::is_none")]
    pub artifact_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<JobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

/// One side of a comparison: what it was asked as, and what the machine built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmRecord {
    pub name: String,
    /// The ref the machine fetched, None for the tree sent from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched: Option<String>,
    #[serde(default)]
    pub revisions: Pairs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeded: Option<String>,
}

/// What was measured, one line of `runs.jsonl` per run. The field order is the line's.
///
/// # Assumptions
///
/// - A run has at least one rep.
///
/// # Guarantees
///
/// - A line [`RunRecord::to_line`] wrote, or that a released dibs appended to `runs.jsonl`,
///   writes back byte for byte. Other JSON that reads, with its keys in another order, its
///   defaults written out or fields it does not know, does not.
/// - Control characters other than newline and tab are written as `\u00XX`, as every line
///   already in a `runs.jsonl` has them.
/// - A field a line leaves out reads as its default, one rep for `rep_count`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// When the run was recorded, since the epoch.
    #[serde(rename = "t")]
    pub when_secs: u64,
    pub verb: RunVerb,
    pub label: String,
    /// The repo's identity, which the label only starts with.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    /// The worktree it came from, when that is not the repo's own checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default)]
    pub recipe: String,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub isolation: String,
    #[serde(default)]
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineName>,
    /// The series its machine measures under, when the machine names one: shared by machines
    /// of one kind, so a run on a new one compares with runs on the last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Alias>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<String>,
    /// Why this did not fit a recipe, for the ad-hoc verbs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The sibling target directory a new tree's was copied from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeded: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchId>,
    /// A comparison's `@` as it was given, such as `main..local`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<String>,
    /// A comparison's arms, in the order given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arms: Vec<ArmRecord>,
    #[serde(
        rename = "reps",
        default = "RunRecord::single_rep",
        skip_serializing_if = "RunRecord::is_single_rep"
    )]
    pub rep_count: u32,
    /// Measured with `--anyway` over a target another tree had built into.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub anyway: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new_series: bool,
    /// The value each of the recipe's `fresh` variables had in this run.
    #[serde(default, skip_serializing_if = "Pairs::is_empty")]
    pub fresh: Pairs,
    /// The image the run's builds ran in, by digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// What the machine read of itself as the measurement took the lock.
    #[serde(default, skip_serializing_if = "Pairs::is_empty")]
    pub state: Pairs,
    /// The recipe's parameters, read by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// The recipe's steps, in the order written.
    #[serde(default)]
    pub procedure: Vec<ProcedureStep>,
    /// Empty for a comparison, whose arms carry their own.
    #[serde(default)]
    pub revisions: Pairs,
    /// The jobs, in the order they started.
    #[serde(default)]
    pub steps: Vec<StepRecord>,
    /// Absent from a line written before outcomes were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

impl RunRecord {
    /// Whether the run failed, from its outcome, or from its steps on a line without one.
    pub fn failed(&self) -> bool {
        self.outcome
            .unwrap_or_else(|| Outcome::of_steps(&self.steps))
            == Outcome::Failed
    }

    /// The line `runs.jsonl` holds for this run, without its newline.
    pub fn to_line(&self) -> String {
        let mut line = Vec::new();
        let mut serializer = serde_json::Serializer::with_formatter(&mut line, RunLineFormatter);
        self.serialize(&mut serializer)
            .expect("a run record serializes");
        String::from_utf8(line).expect("JSON is UTF-8")
    }
}

impl RunRecord {
    fn single_rep() -> u32 {
        1
    }

    fn is_single_rep(rep_count: &u32) -> bool {
        *rep_count <= 1
    }
}

impl FromStr for RunRecord {
    type Err = serde_json::Error;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(line)
    }
}

/// Writes a [`RunRecord`]'s line as compact JSON, with the escapes its Guarantees name.
struct RunLineFormatter;

impl Formatter for RunLineFormatter {
    fn write_char_escape<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        char_escape: CharEscape,
    ) -> io::Result<()> {
        let escaped: &[u8] = match char_escape {
            CharEscape::Quote => b"\\\"",
            CharEscape::ReverseSolidus => b"\\\\",
            CharEscape::Solidus => b"/",
            CharEscape::LineFeed => b"\\n",
            CharEscape::Tab => b"\\t",
            CharEscape::Backspace => b"\\u0008",
            CharEscape::FormFeed => b"\\u000c",
            CharEscape::CarriageReturn => b"\\u000d",
            CharEscape::AsciiControl(byte) => {
                return write!(writer, "\\u{byte:04x}");
            }
        };
        writer.write_all(escaped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECIPE: &str = r#"{"t":1759300000,"verb":"build","label":"app/build/rec","repo":"app","recipe":"rec","fingerprint":"2593baf3b5dbbcce","isolation":"machine","backend":"dibs","procedure":[{"lock":"shared","run":"true"}],"revisions":{"app":"local:abc1234+dirty-0123456789ab"},"steps":[{"lock":"shared","status":0,"seconds":1,"job":"20261001-120000-4242","log":"host:/r/scratch/jobs/20261001-120000-4242/log"}],"outcome":"ok"}"#;
    const COMPARISON: &str = r#"{"t":1,"verb":"bench","label":"app/bench/b@gpu0","repo":"app","variant":"app-wt","recipe":"b","fingerprint":"f","isolation":"machine","backend":"dibs","machine":"m1","device":"gpu0","seeded":"t1","batch":"b1","refs":"main..local","arms":[{"name":"main","fetched":"origin/main","revisions":{"app":"aaa","dep":"ddd"}},{"name":"local","revisions":{"app":"bbb"},"seeded":"t2"}],"reps":2,"anyway":true,"new_series":true,"fresh":{"STORE":"x-r1"},"state":{"gov":"performance"},"params":{"a":"1","b":"two"},"procedure":[{"lock":"shared","run":"export K='v'; cargo build"},{"lock":"exclusive","run":"cargo bench"}],"revisions":{},"steps":[{"lock":"exclusive","status":0,"seconds":9,"arm":"main","rep":1,"artifacts":2,"job":"j1","built":"3","log":"l1"},{"lock":"exclusive","status":1,"seconds":8,"arm":"local","rep":2}],"outcome":"failed"}"#;
    const RAW: &str = r#"{"t":42,"verb":"raw","label":"raw","recipe":"","fingerprint":"","isolation":"machine","backend":"dibs","reason":"no recipe \"fits\"\tyet\nat all\u000d\u0001\u0008\u000c","procedure":[{"lock":"shared","run":"echo 'a\\b'"}],"revisions":{},"steps":[{"lock":"shared","status":0,"seconds":0}],"outcome":"ok"}"#;

    #[test]
    fn the_lines_runs_jsonl_holds_read_and_write_back_byte_for_byte() {
        for line in [RECIPE, COMPARISON, RAW] {
            let record: RunRecord = line.parse().unwrap();
            assert_eq!(record.to_line(), line);
        }
    }

    #[test]
    fn named_values_keep_the_order_they_were_recorded_in() {
        let line = r#"{"t":1,"verb":"build","label":"l","revisions":{"dep":"d","app":"a"}}"#;
        let record: RunRecord = line.parse().unwrap();
        let names: Vec<&str> = record.revisions.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["dep", "app"]);
    }

    #[test]
    fn named_values_sorted_keep_a_repeated_name_s_last_value() {
        let pairs = Pairs::from(vec![
            ("b".to_string(), "1".to_string()),
            ("a".to_string(), "2".to_string()),
            ("b".to_string(), "3".to_string()),
        ]);
        assert_eq!(
            pairs.sorted().into_iter().collect::<Vec<_>>(),
            [("a", "2"), ("b", "3")]
        );
    }

    #[test]
    fn a_recorded_outcome_decides_failed_over_the_steps() {
        let line = r#"{"t":1,"verb":"build","label":"l","steps":[{"lock":"shared","status":3,"seconds":1}],"outcome":"ok"}"#;
        let record: RunRecord = line.parse().unwrap();
        assert!(!record.failed());
    }

    #[test]
    fn a_line_from_before_outcomes_were_recorded_fails_by_its_steps() {
        let line = r#"{"t":1,"verb":"build","label":"l","steps":[{"lock":"shared","status":3,"seconds":1}]}"#;
        let record: RunRecord = line.parse().unwrap();
        assert_eq!((record.outcome, record.rep_count), (None, 1));
        assert!(record.failed());
    }

    #[test]
    fn a_run_of_no_steps_is_ok() {
        assert_eq!(Outcome::of_steps(&[]), Outcome::Ok);
    }
}
