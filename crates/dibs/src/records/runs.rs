//! Reading the run records back.
//!
//! Recording provenance is half of it. The half that matters is being able to ask whether two
//! numbers are comparable, because a label alone never answered that: the same name can cover
//! two different procedures at two different refs, and averaging across that is the failure
//! the history exists to prevent.
//!
//! So this does not just list. It says when a label's runs stopped being comparable, and
//! where.

use crate::{
    paths::{FileError, Paths},
    records::{Kept, RecordsError},
};
use dibs_format::{Lock, Pairs, RunRecord, RunVerb, Span};
use std::{collections::BTreeMap, io::Write as _, path::PathBuf};

/// The run records this computer wrote, one line per run.
pub struct RunLog {
    path: PathBuf,
}

impl RunLog {
    pub fn here() -> Result<RunLog, RecordsError> {
        let path = Paths::from_env()
            .runs()
            .ok_or(RecordsError::NoHome(Kept::Runs))?;
        Ok(RunLog { path })
    }

    pub fn append(&self, run: &RunRecord) -> Result<(), RecordsError> {
        let path = &self.path;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(FileError::at(dir))?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(FileError::at(path))?;
        Ok(writeln!(f, "{}", run.to_line()).map_err(FileError::at(path))?)
    }

    pub fn runs(&self) -> Result<Runs, RecordsError> {
        let path = &self.path;
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Runs::default()),
            Err(e) => return Err(FileError::new(path, e).into()),
        };
        let records = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(parse_line)
            .collect();
        Ok(Runs { records })
    }
}

/// The runs recorded, oldest first.
#[derive(Default)]
pub struct Runs {
    records: Vec<Record>,
}

struct Record {
    pub when: u64,
    pub verb: RunVerb,
    pub label: String,
    pub variant: Option<String>,
    pub fingerprint: String,
    pub machine: Option<String>,
    pub params: Vec<(String, String)>,
    pub state: Vec<(String, String)>,
    pub revisions: Vec<(String, String)>,
    pub procedure: Vec<(String, String)>,
    /// Every step's seconds, and the exclusive steps' alone, which are the measurement. A job
    /// that queued for twenty minutes did not take twenty minutes, so neither counts the wait.
    pub seconds: u64,
    pub measured: Option<u64>,
    /// The exclusive seconds of each rep, keyed by arm: one sample for a run without reps, and
    /// the empty arm for a run that compared nothing.
    pub samples: Vec<(String, u64)>,
    /// A comparison's `@`, and each arm's name and revisions in the order they were given.
    pub refs: Option<String>,
    pub arms: Vec<(String, Vec<(String, String)>)>,
    pub reps: u64,
    pub failed: bool,
    pub anyway: bool,
    pub new_series: bool,
    /// The names of the variables the run was given a value of its own for.
    pub fresh: Vec<String>,
    pub reason: Option<String>,
    pub seeded: Option<String>,
}

/// A line that does not parse is a corrupted tail, since only this program writes the file,
/// so it is skipped rather than fatal.
fn parse_line(line: &str) -> Option<Record> {
    line.parse::<RunRecord>().ok().map(Record::of)
}

impl Record {
    /// Named values by name, the order every report lists them in.
    fn sorted(pairs: &Pairs) -> Vec<(String, String)> {
        pairs
            .sorted()
            .into_iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    fn of(run: RunRecord) -> Record {
        let failed = run.failed();
        let mut per_rep: BTreeMap<(String, u32), u64> = BTreeMap::new();
        for step in run.steps.iter().filter(|s| s.lock == Lock::Exclusive) {
            let arm = step.arm.clone().unwrap_or_default();
            *per_rep.entry((arm, step.rep.unwrap_or(1))).or_default() += step.seconds;
        }
        let samples: Vec<(String, u64)> =
            per_rep.into_iter().map(|((arm, _), s)| (arm, s)).collect();
        let unarmed = samples
            .iter()
            .filter(|(a, _)| a.is_empty())
            .map(|(_, s)| *s);
        Record {
            when: run.when,
            verb: run.verb,
            label: run.label,
            variant: run.variant,
            fingerprint: run.fingerprint,
            machine: run.machine.map(|m| m.to_string()),
            params: run.params.into_iter().collect(),
            state: Record::sorted(&run.state),
            revisions: Record::sorted(&run.revisions),
            procedure: run
                .procedure
                .into_iter()
                .map(|p| (p.lock.as_str().to_string(), p.run))
                .collect(),
            seconds: run.steps.iter().map(|s| s.seconds).sum(),
            measured: Spread::of(unarmed.collect()).map(|s| s.median),
            samples,
            refs: run.refs,
            arms: run
                .arms
                .iter()
                .map(|a| (a.name.clone(), Record::sorted(&a.revisions)))
                .collect(),
            reps: u64::from(run.reps),
            failed,
            anyway: run.anyway,
            new_series: run.new_series,
            fresh: run.fresh.sorted().keys().map(|k| k.to_string()).collect(),
            reason: run.reason,
            seeded: run.seeded,
        }
    }
}

/// The median of some seconds, and the lowest and highest.
struct Spread {
    median: u64,
    low: u64,
    high: u64,
}

impl Spread {
    /// None for no samples.
    fn of(mut secs: Vec<u64>) -> Option<Spread> {
        secs.sort_unstable();
        let n = secs.len();
        (n > 0).then(|| Spread {
            median: (secs[(n - 1) / 2] + secs[n / 2]) / 2,
            low: secs[0],
            high: secs[n - 1],
        })
    }
}

// A record's label is repo/verb/recipe@device, and the name anyone has in hand is the recipe
// alone, which is what --label and the recipe file both call it. Matching whole path prefixes
// only answers "nothing recorded" to the one query a person types, and that reads as the
// provenance never having been written rather than as a query that missed.
fn matches(label: &str, query: &str) -> bool {
    if label == query {
        return true;
    }
    let path = label.split_once('@').map_or(label, |(p, _)| p);
    path == query
        || path.starts_with(&format!("{query}/"))
        || path.rsplit('/').next() == Some(query)
}

/// Seconds east of UTC here, asked once of `date` rather than of a time zone database.
fn local_offset() -> i64 {
    let out = std::process::Command::new("date").arg("+%z").output().ok();
    let z = out
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let (sign, digits) = match z.split_at_checked(1) {
        Some(("-", d)) => (-1, d),
        Some(("+", d)) => (1, d),
        _ => return 0,
    };
    let n: i64 = digits.parse().unwrap_or(0);
    sign * (n / 100 * 3600 + n % 100 * 60)
}

/// `YYYY-MM-DD HH:MM` for seconds since the epoch, by the days-to-civil conversion.
pub fn date(t: i64) -> String {
    let day = Span::DAY.0 as i64;
    let (days, secs) = (t.div_euclid(day), t.rem_euclid(day));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        secs / 60 % 60
    )
}

fn words(kv: &[(String, String)], join: &str) -> String {
    kv.iter()
        .map(|(k, v)| format!("{k}{join}{v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn short(run: &str) -> String {
    match run.char_indices().nth(90) {
        Some((i, _)) => format!("{}...", &run[..i]),
        None => run.to_string(),
    }
}

/// What two measured runs share when they differ in nothing dibs can see.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct SameCode<'a> {
    label: &'a str,
    fingerprint: &'a str,
    machine: &'a str,
    revisions: String,
    params: String,
    state: String,
}

impl Runs {
    pub fn report(&self, only: Option<&str>, limit: usize, all: bool) -> String {
        let records = &self.records;
        let mut out = String::new();
        let picked: Vec<&Record> = records
            .iter()
            .filter(|r| only.is_none_or(|l| matches(&r.label, l)))
            .collect();
        let hidden = if all {
            0
        } else {
            picked.iter().filter(|r| r.failed).count()
        };
        let shown: Vec<&Record> = picked
            .iter()
            .rev()
            .filter(|r| all || !r.failed)
            .take(limit)
            .copied()
            .collect();

        if shown.is_empty() {
            return match only {
                // Saying what is there separates a query that missed from a record that was never
                // written, which are the same sentence otherwise and lead opposite ways.
                Some(l) if !picked.is_empty() => {
                    format!(
                        "nothing but failed runs for {l}.\n  dibs runs {l} --all   lists them.\n"
                    )
                }
                Some(l) if !records.is_empty() => format!(
                    "nothing recorded for {l}, out of {} runs recorded.\n  dibs runs   lists them; a label is repo/verb/recipe.\n",
                    records.len()
                ),
                Some(l) => format!("nothing recorded for {l}, and nothing recorded at all yet.\n"),
                None => "nothing recorded yet\n".to_string(),
            };
        }

        let offset = local_offset();
        let width = |f: fn(&Record) -> usize| shown.iter().map(|r| f(r)).max().unwrap_or(0);
        let machine_w = width(|r| r.machine.as_deref().map_or(1, str::len));
        let label_w = width(|r| r.label.len());
        for r in &shown {
            let (secs, lock) = match r.measured {
                Some(m) => (m, "exclusive"),
                None => (r.seconds, "shared"),
            };
            let spread = Spread::of(r.samples.iter().map(|(_, s)| *s).collect())
                .filter(|_| r.arms.is_empty() && r.reps > 1);
            let extra = [
                r.variant.as_ref().map(|v| format!("from {v}")),
                spread.map(|s| format!("median of {} reps, {}s to {}s", r.reps, s.low, s.high)),
                (!r.params.is_empty()).then(|| words(&r.params, "=")),
                r.seeded.as_ref().map(|s| format!("seeded from {s}")),
                r.anyway.then(|| "measured with --anyway".to_string()),
                r.new_series.then(|| "a new series starts here".to_string()),
                r.failed.then(|| "FAILED".to_string()),
            ];
            let extra: String = extra
                .into_iter()
                .flatten()
                .map(|e| format!("  {e}"))
                .collect();
            if !r.arms.is_empty() {
                out.push_str(&format!(
                    "{}  {:<machine_w$}  {:<label_w$}  {} arms, {} each{extra}\n",
                    date(r.when as i64 + offset),
                    r.machine.as_deref().unwrap_or("-"),
                    r.label,
                    r.refs.as_deref().unwrap_or("compared"),
                    if r.reps == 1 {
                        "measured once".to_string()
                    } else {
                        format!("{} reps", r.reps)
                    },
                ));
                let name_w = r.arms.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
                for (name, revisions) in &r.arms {
                    let secs = Spread::of(
                        r.samples
                            .iter()
                            .filter(|(a, _)| a == name)
                            .map(|(_, s)| *s)
                            .collect(),
                    );
                    out.push_str(&format!(
                    "    {name:<name_w$}  {}  {}\n",
                    words(revisions, "@"),
                    match secs {
                        Some(s) if s.low != s.high =>
                            format!("median {}s exclusive, {}s to {}s", s.median, s.low, s.high),
                        Some(s) => format!("{}s exclusive", s.median),
                        None => "nothing measured".to_string(),
                    }
                ));
                }
                continue;
            }
            out.push_str(&format!(
                "{}  {:<machine_w$}  {:<label_w$} {:>6}s {:<9}  {}{extra}\n",
                date(r.when as i64 + offset),
                r.machine.as_deref().unwrap_or("-"),
                r.label,
                secs,
                lock,
                words(&r.revisions, "@"),
            ));
        }

        // Runs that differ in nothing dibs can see: the spread among them is the noise any
        // difference elsewhere has to beat. State is part of the key, so two governors are two lines.
        // A comparison's arms are set against each other in its own record instead.
        let mut repeated: BTreeMap<SameCode, Vec<u64>> = BTreeMap::new();
        for r in picked.iter().filter(|r| !r.failed && r.arms.is_empty()) {
            if r.measured.is_some() {
                let key = SameCode {
                    label: r.label.as_str(),
                    fingerprint: r.fingerprint.as_str(),
                    machine: r.machine.as_deref().unwrap_or("-"),
                    revisions: words(&r.revisions, "@"),
                    params: words(&r.params, "="),
                    state: words(&r.state, "="),
                };
                repeated
                    .entry(key)
                    .or_default()
                    .extend(r.samples.iter().map(|(_, s)| *s));
            }
        }
        let repeated: Vec<_> = repeated.into_iter().filter(|(_, v)| v.len() > 1).collect();
        if !repeated.is_empty() {
            out.push_str("\nrepeated on the same code, measured step only:\n");
            for (
                SameCode {
                    label,
                    machine,
                    revisions: revs,
                    params,
                    state,
                    ..
                },
                secs,
            ) in repeated
            {
                let n = secs.len();
                let Some(Spread {
                    median: m,
                    low: lo,
                    high: hi,
                }) = Spread::of(secs)
                else {
                    continue;
                };
                let context: String = [params, state]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(", {s}"))
                    .collect();
                out.push_str(&format!("  {label} on {machine} at {revs}{context}: measured {n} times, median {m}s, {lo}s to {hi}s\n"));
            }
        }

        // The point of recording the fingerprint. A label whose procedure changed has a history
        // that is two histories, and nothing else would say so. Compared within one set of values,
        // since a parameter changes the commands on purpose. A procedure that never ran cleanly left
        // no numbers to mix, and a shell, which older records filed as a build, is a one-off.
        let mut by_label: BTreeMap<(&str, String), Vec<&Record>> = BTreeMap::new();
        for r in picked
            .iter()
            .filter(|r| !r.failed && r.verb != RunVerb::Shell && !r.label.ends_with("/shell"))
        {
            by_label
                .entry((&r.label, words(&r.params, "=")))
                .or_default()
                .push(r);
        }
        let mut split = String::new();
        for ((label, params), rs) in &by_label {
            let label = match params.is_empty() {
                true => label.to_string(),
                false => format!("{label} with {params}"),
            };
            let mut latest: Vec<&Record> = Vec::new();
            for r in rs.iter().rev() {
                if !latest.iter().any(|l| l.fingerprint == r.fingerprint) {
                    latest.push(r);
                }
            }
            if let [newer, older, ..] = latest[..] {
                let n = newer.procedure.len().max(older.procedure.len());
                let step = |r: &Record, i: usize| {
                    r.procedure
                        .get(i)
                        .map(|(l, run)| format!("[{l}] {}", short(run)))
                };
                let say = |s: Option<String>| {
                    s.map(|s| format!("`{s}`"))
                        .unwrap_or_else(|| "nothing".into())
                };
                let fresh = |r: &Record| (!r.fresh.is_empty()).then(|| r.fresh.join(", "));
                let change = (0..n)
                    .find(|&i| step(older, i) != step(newer, i))
                    .map(|i| {
                        format!(
                            " The latest change is step {}: {} became {}.",
                            i + 1,
                            say(step(older, i)),
                            say(step(newer, i))
                        )
                    })
                    .or_else(|| {
                        (older.fresh != newer.fresh).then(|| {
                        format!(
                            " The latest change is what gets a fresh value each run: {} became {}.",
                            say(fresh(older)),
                            say(fresh(newer))
                        )
                    })
                    });
                split.push_str(&format!(
                "  {label}: {} different recipes have run under this name, and runs under one are not \
                 comparable with runs under another.{}\n",
                latest.len(),
                change.unwrap_or_default()
            ));
            }
        }
        if !split.is_empty() {
            out.push('\n');
            out.push_str(&split);
        }
        if hidden > 0 {
            out.push_str(&format!(
                "\n{hidden} failed run{} not shown:  dibs runs{} --all\n",
                if hidden == 1 { " is" } else { "s are" },
                only.map(|l| format!(" {l}")).unwrap_or_default()
            ));
        }
        out
    }

    /// What did not fit, grouped. The escape hatch is instrumented rather than discouraged: a
    /// reason that keeps recurring is a specification for the next verb, written by whoever needed
    /// it rather than guessed at here.
    ///
    /// Raw usage is not a number to drive to zero. A one-off sweep written for one investigation
    /// is a benchmark that will never run again, and forcing a recipe for it is friction with no
    /// payoff. The failure to watch for is the opposite: a reason that recurs and nobody promoted.
    pub fn gaps(&self) -> String {
        let mut counts: BTreeMap<&str, (usize, Vec<&str>)> = BTreeMap::new();
        for r in &self.records {
            if let Some(reason) = &r.reason {
                let e = counts.entry(reason.as_str()).or_insert((0, Vec::new()));
                e.0 += 1;
                if !e.1.contains(&r.label.as_str()) {
                    e.1.push(&r.label);
                }
            }
        }
        if counts.is_empty() {
            return "Nothing has needed the escape hatch. Either everything fits a recipe, or nobody\nis using the verbs yet.\n"
            .to_string();
        }
        let mut ordered: Vec<(&str, usize, Vec<&str>)> =
            counts.into_iter().map(|(k, (n, l))| (k, n, l)).collect();
        ordered.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

        let mut out = String::from("What did not fit a recipe:\n\n");
        for (reason, n, labels) in &ordered {
            out.push_str(&format!(
                "  {n:>3}x  {reason}\n       {}\n",
                labels.join(", ")
            ));
        }
        let repeated = ordered.iter().filter(|(_, n, _)| *n > 1).count();
        if repeated > 0 {
            out.push_str(&format!(
                "\n{repeated} of these have happened more than once. Those are the ones worth a \
             recipe;\nthe rest are one-offs and are meant to stay here.\n"
            ));
        }
        out
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The label people have is the recipe name; the label recorded is repo/verb/recipe@device.
    // When those did not meet, the answer was "nothing recorded", which reads as the record
    // never being written and sends someone to look for a provenance bug that is not there.
    #[test]
    fn a_label_is_matched_by_every_name_it_is_known_by() {
        let full = "cubek/bench/reduce-topk5@gpu:rtx4070tisuper";
        for q in [
            "cubek",
            "cubek/bench",
            "cubek/bench/reduce-topk5",
            "reduce-topk5",
            full,
        ] {
            assert!(matches(full, q), "{q} should reach {full}");
        }
        for q in [
            "topk5",
            "cubek/test",
            "reduce-topk5@gpu:other",
            "gpu:rtx4070tisuper",
        ] {
            assert!(!matches(full, q), "{q} should not reach {full}");
        }
    }

    #[test]
    fn a_comparison_lists_each_arm_with_its_median_across_reps() {
        let line = r#"{"t":1,"verb":"bench","label":"cubek/bench/gemm","repo":"cubek","fingerprint":"f","machine":"m1","refs":"main..local","arms":[{"name":"base","fetched":"626548f5aa","revisions":{"cubek":"626548f5aa"}},{"name":"local","revisions":{"cubek":"local:8f12+dirty-ab"}}],"reps":2,"procedure":[],"revisions":{},"steps":[{"lock":"shared","status":0,"seconds":90,"arm":"base"},{"lock":"exclusive","status":0,"seconds":40,"arm":"base","rep":1},{"lock":"exclusive","status":0,"seconds":30,"arm":"local","rep":1},{"lock":"exclusive","status":0,"seconds":32,"arm":"local","rep":2},{"lock":"exclusive","status":0,"seconds":42,"arm":"base","rep":2}],"outcome":"ok"}"#;
        let out = Runs {
            records: vec![parse_line(line).unwrap()],
        }
        .report(None, 30, false);
        assert!(
            out.contains("cubek/bench/gemm  main..local arms, 2 reps each"),
            "{out}"
        );
        assert!(
            out.contains("    base   cubek@626548f5aa  median 41s exclusive, 40s to 42s"),
            "{out}"
        );
        assert!(
            out.contains("    local  cubek@local:8f12+dirty-ab  median 31s exclusive, 30s to 32s"),
            "{out}"
        );
        assert!(
            !out.contains("repeated on the same code"),
            "the arms are compared in their own record: {out}"
        );
    }

    #[test]
    fn the_reps_of_one_run_are_its_spread() {
        let line = r#"{"t":1,"verb":"bench","label":"cubek/bench/gemm","fingerprint":"f","machine":"m1","reps":3,"procedure":[],"revisions":{"cubek":"abc"},"steps":[{"lock":"shared","status":0,"seconds":90},{"lock":"exclusive","status":0,"seconds":12,"rep":1},{"lock":"exclusive","status":0,"seconds":10,"rep":2},{"lock":"exclusive","status":0,"seconds":11,"rep":3}],"outcome":"ok"}"#;
        let out = Runs {
            records: vec![parse_line(line).unwrap()],
        }
        .report(None, 30, false);
        assert!(
            out.contains("    11s exclusive  cubek@abc  median of 3 reps, 10s to 12s"),
            "{out}"
        );
        assert!(
            out.contains("measured 3 times, median 11s, 10s to 12s"),
            "{out}"
        );
    }

    #[test]
    fn a_query_that_missed_does_not_read_as_an_empty_record() {
        let recs = Runs {
            records: vec![parse_line(A).unwrap()],
        };
        let out = recs.report(Some("no-such-label"), 30, false);
        assert!(out.contains("out of 1 runs recorded"), "{out}");
        assert!(
            Runs { records: vec![] }
                .report(Some("x"), 30, false)
                .contains("nothing recorded at all")
        );
    }

    const A: &str = r#"{"t":100,"verb":"bench","label":"cubek/gemm","fingerprint":"aaa","isolation":"machine","backend":"dibs","procedure":[{"lock":"shared","run":"cargo build"},{"lock":"exclusive","run":"cargo bench"}],"revisions":{"cubek":"abc123"},"steps":[{"lock":"shared","status":0,"seconds":30},{"lock":"exclusive","status":0,"seconds":120}]}"#;
    const B: &str = r#"{"t":200,"verb":"bench","label":"cubek/gemm","fingerprint":"bbb","isolation":"machine","backend":"dibs","revisions":{"cubek":"def456"},"steps":[{"lock":"exclusive","status":1,"seconds":5}]}"#;

    fn v2(
        t: u64,
        fingerprint: &str,
        bench: &str,
        measured: u64,
        state: &str,
        outcome: &str,
    ) -> Record {
        parse_line(&format!(
            r#"{{"t":{t},"verb":"bench","label":"cubek/bench/gemm","repo":"cubek","fingerprint":"{fingerprint}","machine":"m1","state":{{"governor":"{state}"}},"procedure":[{{"lock":"shared","run":"cargo build --release"}},{{"lock":"exclusive","run":"{bench}"}}],"revisions":{{"cubek":"abc123"}},"steps":[{{"lock":"shared","status":0,"seconds":30,"job":"1-1","built":"nothing"}},{{"lock":"exclusive","status":0,"seconds":{measured},"job":"1-2","log":"m1:/j/1-2/log"}}],"outcome":"{outcome}"}}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_record_reads_back_whole() {
        let r = parse_line(A).unwrap();
        assert_eq!(r.label, "cubek/gemm");
        assert_eq!(r.revisions, vec![("cubek".into(), "abc123".into())]);
        assert_eq!((r.seconds, r.measured), (150, Some(120)));
        assert!(!r.failed);
    }

    #[test]
    fn a_failing_step_marks_a_run_written_before_outcomes_were() {
        assert!(parse_line(B).unwrap().failed);
    }

    #[test]
    fn a_failed_run_is_left_out_unless_asked_for() {
        let recs = Runs {
            records: vec![
                v2(1, "f", "cargo bench", 9, "performance", "ok"),
                v2(2, "f", "cargo bench", 1, "performance", "failed"),
            ],
        };
        let out = recs.report(None, 10, false);
        assert_eq!(
            out.lines()
                .filter(|l| l.contains("cubek/bench/gemm"))
                .count(),
            1,
            "{out}"
        );
        assert!(
            out.contains("1 failed run is not shown:  dibs runs --all"),
            "{out}"
        );
        assert!(recs.report(None, 10, true).contains("FAILED"));
    }

    #[test]
    fn a_row_says_when_where_and_which_lock_its_number_is_from() {
        let out = Runs {
            records: vec![v2(1789745748, "f", "cargo bench", 42, "performance", "ok")],
        }
        .report(None, 10, false);
        assert!(out.contains(" m1 "), "{out}");
        assert!(
            out.contains("42s exclusive"),
            "the measured step, not the build before it: {out}"
        );
    }

    #[test]
    fn repeated_runs_give_a_median_and_a_second_state_is_a_second_line() {
        let recs = Runs {
            records: vec![
                v2(1, "f", "cargo bench", 10, "performance", "ok"),
                v2(2, "f", "cargo bench", 14, "performance", "ok"),
                v2(3, "f", "cargo bench", 11, "performance", "ok"),
                v2(4, "f", "cargo bench", 30, "powersave", "ok"),
                v2(5, "f", "cargo bench", 31, "powersave", "ok"),
            ],
        };
        let out = recs.report(None, 10, false);
        assert!(
            out.contains("governor=performance: measured 3 times, median 11s, 10s to 14s"),
            "{out}"
        );
        assert!(
            out.contains("governor=powersave: measured 2 times, median 30s, 30s to 31s"),
            "{out}"
        );
    }

    #[test]
    fn a_worktree_s_run_names_it_and_still_repeats_the_repo_s() {
        let mut from_worktree = v2(2, "f", "cargo bench", 12, "performance", "ok");
        from_worktree.variant = Some("topk-packed".into());
        let out = Runs {
            records: vec![
                v2(1, "f", "cargo bench", 10, "performance", "ok"),
                from_worktree,
            ],
        }
        .report(None, 10, false);
        assert_eq!(out.matches("from topk-packed").count(), 1, "{out}");
        assert!(out.contains(": measured 2 times, median 11s"), "{out}");
    }

    #[test]
    fn a_label_whose_recipe_changed_names_the_step() {
        let recs = Runs {
            records: vec![
                v2(1, "f1", "cargo bench", 9, "performance", "ok"),
                v2(2, "f2", "cargo bench -- --quick", 9, "performance", "ok"),
            ],
        };
        let out = recs.report(None, 10, false);
        assert!(
            out.contains("2 different recipes have run under this name"),
            "{out}"
        );
        assert!(
            out.contains(
                "step 2: `[exclusive] cargo bench` became `[exclusive] cargo bench -- --quick`"
            ),
            "{out}"
        );
    }

    // A sweep changes the commands on purpose, and says so in its parameters.
    #[test]
    fn two_values_of_a_parameter_are_not_two_recipes() {
        let point = |t: u64, f: &str, samples: &str| {
            parse_line(&format!(r#"{{"t":{t},"verb":"build","label":"a/build/p","fingerprint":"{f}","params":{{"samples":"{samples}"}},"procedure":[],"revisions":{{}},"steps":[]}}"#)).unwrap()
        };
        let out = Runs {
            records: vec![point(1, "f10", "10"), point(2, "f30", "30")],
        }
        .report(None, 10, false);
        assert!(!out.contains("different recipes"), "{out}");
        let out = Runs {
            records: vec![point(1, "f10", "10"), point(2, "g10", "10")],
        }
        .report(None, 10, false);
        assert!(
            out.contains("a/build/p with samples=10: 2 different recipes"),
            "{out}"
        );
    }

    #[test]
    fn a_recipe_that_changed_only_what_runs_fresh_says_so() {
        let mut older = v2(1, "f1", "cargo bench", 9, "performance", "ok");
        let mut newer = v2(2, "f2", "cargo bench", 9, "performance", "ok");
        older.fresh = vec![];
        newer.fresh = vec!["CUBECL_ENVIRONMENT".into()];
        let out = Runs {
            records: vec![older, newer],
        }
        .report(None, 10, false);
        assert!(
            out.contains("fresh value each run: nothing became `CUBECL_ENVIRONMENT`"),
            "{out}"
        );
    }

    // Nothing was measured under the one that failed, so there is no second history to warn of.
    #[test]
    fn a_recipe_that_never_ran_cleanly_is_not_a_second_history() {
        let recs = Runs {
            records: vec![
                v2(1, "f1", "cargo bench", 9, "performance", "ok"),
                v2(2, "f2", "cargo bench --x", 1, "performance", "failed"),
            ],
        };
        assert!(!recs.report(None, 10, true).contains("different recipes"));
    }

    #[test]
    fn one_recipe_throughout_says_nothing() {
        let recs = Runs {
            records: vec![parse_line(A).unwrap()],
        };
        assert!(!recs.report(None, 10, false).contains("different recipes"));
    }

    #[test]
    fn a_date_is_civil_time() {
        assert_eq!(date(0), "1970-01-01 00:00");
        assert_eq!(date(1789745748), "2026-09-18 15:35");
        assert_eq!(date(951782400), "2000-02-29 00:00");
    }

    #[test]
    fn gaps_ranks_what_recurs_and_says_which_to_promote() {
        let mk = |reason: &str| {
            let line = format!(
                r#"{{"t":1,"verb":"raw","label":"raw","fingerprint":"","isolation":"machine","reason":"{reason}","backend":"dibs","revisions":{{}},"steps":[]}}"#
            );
            parse_line(&line).unwrap()
        };
        let recs = Runs {
            records: vec![
                mk("git bisect across builds"),
                mk("git bisect across builds"),
                mk("one off"),
            ],
        };
        let out = recs.gaps();
        assert!(out.starts_with("What did not fit"));
        // The recurring one first, because that is the one worth a recipe.
        let bisect = out.find("git bisect").unwrap();
        let oneoff = out.find("one off").unwrap();
        assert!(
            bisect < oneoff,
            "the recurring reason has to rank above the one-off"
        );
        assert!(out.contains("1 of these have happened more than once"));
    }

    #[test]
    fn no_reasons_recorded_is_not_an_error() {
        assert!(
            Runs::default()
                .gaps()
                .contains("Nothing has needed the escape hatch")
        );
    }

    #[test]
    fn a_corrupt_tail_is_skipped_rather_than_fatal() {
        assert!(parse_line("{\"t\":1,\"verb\":\"bench\"").is_none());
    }

    #[test]
    fn a_seeded_run_says_where_its_target_came_from() {
        let line = r#"{"t":1,"verb":"build","label":"m/build/x","fingerprint":"f","isolation":"machine","backend":"dibs","seeded":"m-local-abc","revisions":{"m":"abc"},"steps":[{"lock":"shared","status":0,"seconds":9}]}"#;
        assert_eq!(
            parse_line(line).unwrap().seeded.as_deref(),
            Some("m-local-abc")
        );
    }

    // Written by this program and read by this program, with a command in it that has quotes.
    #[test]
    fn a_command_with_quotes_in_it_reads_back() {
        let line = r#"{"t":1,"verb":"bench","label":"x","fingerprint":"f","procedure":[{"lock":"exclusive","run":"echo \"a b\" > \"$T\""}],"revisions":{},"steps":[]}"#;
        assert_eq!(
            parse_line(line).unwrap().procedure[0].1,
            r#"echo "a b" > "$T""#
        );
    }
}
