//! A run as one measurement of a benchmark: the procedure that ran, the
//! conditions it ran under, and one arm per side of a comparison with the revisions it ran at.
//!
//! dibs keeps no result numbers, only how long steps took, so the values come from whoever read
//! them in the run's output, by arm. Without them every arm is written with none, for that
//! reader to fill in before it is recorded.

use dibs_format::{Moment, Pairs, RunRecord};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Measurement {
    pub measured_at: String,
    pub procedure: String,
    pub conditions: Map<String, Json>,
    pub source: String,
    pub arms: Vec<Arm>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Arm {
    pub name: String,
    pub revisions: Map<String, Json>,
    pub values: Vec<Value>,
}

/// One number read from a run's output: a quantity, where along the benchmark's axis, and
/// which rep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Value {
    pub quantity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<f64>,
    #[serde(default = "Value::first_rep")]
    pub rep: u32,
    pub value: f64,
}

impl Value {
    fn first_rep() -> u32 {
        1
    }
}

/// Why runs cannot be written as one measurement.
#[derive(Debug, Clone, PartialEq)]
pub enum Unmeasurable {
    NothingNamed(String),
    Failed(String),
    /// Runs that differ in what makes a series: the procedure, or one of the conditions.
    Differ {
        id: String,
        what: String,
    },
    SeveralComparisons(String),
    ComparedAndNot(String),
    Values(String),
    ValuesForNoArm {
        arm: String,
        arms: Vec<String>,
    },
}

impl fmt::Display for Unmeasurable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unmeasurable::NothingNamed(id) => write!(
                f,
                "no run recorded here has the job or batch {id}: dibs runs lists what ran, and \
                 the trailer of a run names its job"
            ),
            Unmeasurable::Failed(id) => write!(
                f,
                "a run of {id} failed, so its numbers measure nothing: run it again"
            ),
            Unmeasurable::Differ { id, what } => write!(
                f,
                "the runs of {id} differ in {what}, and a measurement holds one procedure under \
                 one set of conditions: name one run's job, as each point of a sweep is a \
                 measurement of its own"
            ),
            Unmeasurable::SeveralComparisons(id) => write!(
                f,
                "{id} holds several comparisons: name the job of one of them"
            ),
            Unmeasurable::ComparedAndNot(id) => write!(
                f,
                "{id} holds a comparison beside single runs: name the job of the comparison, or \
                 of one single run"
            ),
            Unmeasurable::Values(why) => write!(f, "--values: {why}"),
            Unmeasurable::ValuesForNoArm { arm, arms } => write!(
                f,
                "--values names the arm {arm}, and the runs have {}",
                arms.join(", ")
            ),
        }
    }
}

/// The run of one job, or every run of one batch, as one measurement.
pub fn measurement_of(records: &[RunRecord], id: &str) -> Result<Measurement, Unmeasurable> {
    let runs: Vec<&RunRecord> = records
        .iter()
        .filter(|r| {
            r.batch.as_ref().is_some_and(|b| b.as_str() == id)
                || r.steps
                    .iter()
                    .any(|s| s.job.as_ref().is_some_and(|j| j.as_str() == id))
        })
        .collect();
    let Some(first) = runs.first() else {
        return Err(Unmeasurable::NothingNamed(id.to_string()));
    };
    if runs.iter().any(|r| r.failed()) {
        return Err(Unmeasurable::Failed(id.to_string()));
    }
    let differ = |what: &str| Unmeasurable::Differ {
        id: id.to_string(),
        what: what.to_string(),
    };
    if runs.iter().any(|r| r.fingerprint != first.fingerprint) {
        return Err(differ("their procedure"));
    }
    let conditions = conditions_of(first);
    if let Some(theirs) = runs
        .iter()
        .map(|r| conditions_of(r))
        .find(|c| *c != conditions)
    {
        let name = conditions
            .keys()
            .chain(theirs.keys())
            .find(|name| conditions.get(*name) != theirs.get(*name))
            .map_or("their conditions", |name| name.as_str());
        return Err(differ(name));
    }
    let compared = runs.iter().filter(|r| !r.arms.is_empty()).count();
    let arms = match compared {
        0 => arms_by_revision(&runs),
        1 if runs.len() == 1 => first
            .arms
            .iter()
            .map(|a| Arm {
                name: a.name.clone(),
                revisions: object_of(&a.revisions),
                values: Vec::new(),
            })
            .collect(),
        1 => return Err(Unmeasurable::ComparedAndNot(id.to_string())),
        _ => return Err(Unmeasurable::SeveralComparisons(id.to_string())),
    };
    let jobs: Vec<&str> = runs
        .iter()
        .flat_map(|r| {
            r.steps
                .iter()
                .filter_map(|s| s.job.as_ref().map(|j| j.as_str()))
        })
        .collect();
    let source = match &first.batch {
        Some(batch) if runs.len() > 1 => {
            format!("dibs batch {}, jobs {}", batch.as_str(), jobs.join(", "))
        }
        _ => format!("dibs jobs {}", jobs.join(", ")),
    };
    let earliest = runs.iter().map(|r| r.when).min().unwrap_or(first.when);
    let moment = Moment::in_zone(earliest, 0);
    Ok(Measurement {
        measured_at: format!("{}T{}Z", moment.day(), moment.clock()),
        procedure: first
            .procedure
            .iter()
            .map(|step| step.run.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        conditions,
        source,
        arms,
    })
}

/// What a series is kept apart by besides the procedure: the recipe, where it ran, the values
/// it was given, and what the machine read of itself, so a new driver starts a new series.
fn conditions_of(run: &RunRecord) -> Map<String, Json> {
    let mut conditions = Map::new();
    let mut put = |name: &str, value: &str| {
        conditions.insert(name.to_string(), Json::String(value.to_string()));
    };
    put("label", &run.label);
    if let Some(machine) = &run.machine {
        put("machine", machine.as_str());
    }
    if let Some(series) = &run.series {
        put("series", series);
    }
    if let Some(device) = &run.device {
        put("device", device.as_str());
    }
    for (name, value) in &run.params {
        put(name, value);
    }
    for pair in run.state.iter() {
        put(&pair.name, &pair.value);
    }
    conditions
}

fn object_of(pairs: &Pairs) -> Map<String, Json> {
    pairs
        .iter()
        .map(|pair| (pair.name.clone(), Json::String(pair.value.clone())))
        .collect()
}

/// Runs compared by hand, one per side and rep, are told apart by what they ran at: one arm per
/// set of revisions, named by every one of them, since two arms may differ only in a pin.
fn arms_by_revision(runs: &[&RunRecord]) -> Vec<Arm> {
    let mut arms: Vec<Arm> = Vec::new();
    for run in runs {
        let revisions = object_of(&run.revisions);
        if arms.iter().any(|a| a.revisions == revisions) {
            continue;
        }
        let name = run
            .revisions
            .iter()
            .map(|pair| format!("{}@{}", pair.name, short(&pair.value)))
            .collect::<Vec<_>>()
            .join(" ");
        arms.push(Arm {
            name: if name.is_empty() { "run".into() } else { name },
            revisions,
            values: Vec::new(),
        });
    }
    arms
}

/// A commit's first twelve hex digits; a tree sent from here is kept whole, since its name ends
/// in the digest that tells two edits of one commit apart.
fn short(revision: &str) -> String {
    match revision.chars().all(|c| c.is_ascii_hexdigit()) {
        true => revision.chars().take(12).collect(),
        false => revision.to_string(),
    }
}

/// The values read from the run's output, as a JSON object of each arm's values, set on the arms
/// they name.
pub fn with_values(mut measurement: Measurement, text: &str) -> Result<Measurement, Unmeasurable> {
    let by_arm: std::collections::BTreeMap<String, Vec<Value>> = serde_json::from_str(text)
        .map_err(|e| {
            Unmeasurable::Values(format!(
                "{e}; it takes a JSON object of each arm's values, {{\"<arm>\": [{{\"quantity\": \
                 \"TTFT p50\", \"point\": 1, \"rep\": 1, \"value\": 0.9}}]}}"
            ))
        })?;
    let names: Vec<String> = measurement.arms.iter().map(|a| a.name.clone()).collect();
    for (arm, values) in by_arm {
        let Some(found) = measurement.arms.iter_mut().find(|a| a.name == arm) else {
            return Err(Unmeasurable::ValuesForNoArm { arm, arms: names });
        };
        found.values = values;
    }
    Ok(measurement)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(line: &str) -> RunRecord {
        line.parse().expect("the test's line is a run record")
    }

    const COMPARISON: &str = r#"{"t":1793116800,"verb":"bench","label":"app/bench/serve@gpu0","repo":"app","recipe":"serve","fingerprint":"0123456789abcdef","machine":"bench-box","device":"gpu0","batch":"20261027-1","refs":"main..local","arms":[{"name":"base","revisions":{"app":"a1b2c3"}},{"name":"local","revisions":{"app":"local:d4e5f6+dirty-0123456789ab"}}],"reps":2,"params":{"users":"8"},"procedure":[{"lock":"exclusive","run":"cargo bench --bench serve"}],"revisions":{},"steps":[{"lock":"exclusive","status":0,"seconds":30,"arm":"base","rep":1,"job":"20261027-111"},{"lock":"exclusive","status":0,"seconds":30,"arm":"local","rep":1,"job":"20261027-112"}],"outcome":"ok"}"#;

    fn single(job: &str, commit: &str, when: u64) -> String {
        format!(
            r#"{{"t":{when},"verb":"bench","label":"app/bench/serve","repo":"app","recipe":"serve","fingerprint":"0123456789abcdef","machine":"bench-box","batch":"20261027-2","procedure":[{{"lock":"exclusive","run":"cargo bench --bench serve"}}],"revisions":{{"app":"{commit}"}},"steps":[{{"lock":"exclusive","status":0,"seconds":30,"job":"{job}"}}],"outcome":"ok"}}"#
        )
    }

    #[test]
    fn a_comparison_keeps_its_arms_and_their_revisions_and_names_its_jobs() {
        let measured = measurement_of(&[run(COMPARISON)], "20261027-111").unwrap();

        assert_eq!(
            measured
                .arms
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            ["base", "local"]
        );
        assert_eq!(
            measured.arms[1].revisions["app"],
            "local:d4e5f6+dirty-0123456789ab"
        );
        assert_eq!(measured.source, "dibs jobs 20261027-111, 20261027-112");
        assert_eq!(measured.procedure, "cargo bench --bench serve");
        assert_eq!(measured.measured_at, "2026-10-27T16:00:00Z");
    }

    #[test]
    fn the_conditions_name_the_recipe_the_machine_the_card_and_the_values_it_was_given() {
        let measured = measurement_of(&[run(COMPARISON)], "20261027-1").unwrap();

        assert_eq!(
            serde_json::to_string(&measured.conditions).unwrap(),
            r#"{"device":"gpu0","label":"app/bench/serve@gpu0","machine":"bench-box","users":"8"}"#
        );
    }

    #[test]
    fn runs_of_one_batch_at_two_revisions_are_two_arms_from_the_earliest_moment() {
        let records = [
            run(&single("1", "a1b2c3d4e5f6a7", 1793116900)),
            run(&single("2", "ffffff000000", 1793116800)),
            run(&single("3", "ffffff000000", 1793117000)),
            run(&single("4", "a1b2c3d4e5f6a7", 1793117100)),
        ];

        let measured = measurement_of(&records, "20261027-2").unwrap();

        assert_eq!(
            measured
                .arms
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            ["app@a1b2c3d4e5f6", "app@ffffff000000"]
        );
        assert_eq!(measured.source, "dibs batch 20261027-2, jobs 1, 2, 3, 4");
        assert_eq!(measured.measured_at, "2026-10-27T16:00:00Z");
    }

    #[test]
    fn runs_differing_only_in_a_pin_or_in_edits_to_one_commit_are_arms_of_two_names() {
        let pinned = |job: &str, lib: &str| {
            single(job, "a1b2c3d4e5f6a7", 1793116800).replace(
                r#""revisions":{"app":"a1b2c3d4e5f6a7"}"#,
                &format!(r#""revisions":{{"app":"a1b2c3d4e5f6a7","lib":"{lib}"}}"#),
            )
        };
        let records = [
            run(&pinned("1", "1111111111111111")),
            run(&pinned("2", "2222222222222222")),
            run(&single("3", "local:a1b2c3d+dirty-0123456789ab", 1793116800)),
            run(&single("4", "local:a1b2c3d+dirty-ba9876543210", 1793116800)),
        ];

        let measured = measurement_of(&records, "20261027-2").unwrap();

        assert_eq!(
            measured
                .arms
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            [
                "app@a1b2c3d4e5f6 lib@111111111111",
                "app@a1b2c3d4e5f6 lib@222222222222",
                "app@local:a1b2c3d+dirty-0123456789ab",
                "app@local:a1b2c3d+dirty-ba9876543210",
            ]
        );
    }

    #[test]
    fn runs_of_two_procedures_are_refused() {
        let other = single("2", "a1", 1793116800).replace("0123456789abcdef", "fedcba9876543210");

        let refused = measurement_of(
            &[run(&single("1", "a1", 1793116800)), run(&other)],
            "20261027-2",
        );

        assert_eq!(
            refused,
            Err(Unmeasurable::Differ {
                id: "20261027-2".into(),
                what: "their procedure".into()
            })
        );
    }

    #[test]
    fn a_failed_run_is_refused() {
        let failed = single("1", "a1", 1793116800)
            .replace(r#""status":0"#, r#""status":1"#)
            .replace(r#""outcome":"ok""#, r#""outcome":"failed""#);

        let refused = measurement_of(&[run(&failed)], "1");

        assert_eq!(refused, Err(Unmeasurable::Failed("1".into())));
    }

    #[test]
    fn a_job_no_run_names_is_refused() {
        let refused = measurement_of(&[run(COMPARISON)], "nothing");

        assert_eq!(refused, Err(Unmeasurable::NothingNamed("nothing".into())));
    }

    #[test]
    fn values_set_the_arms_they_name_and_take_the_first_rep_when_left_out() {
        let measured = measurement_of(&[run(COMPARISON)], "20261027-1").unwrap();

        let valued = with_values(
            measured,
            r#"{"local": [{"quantity": "TTFT p50", "point": 1, "value": 0.93}]}"#,
        )
        .unwrap();

        assert!(valued.arms[0].values.is_empty());
        assert_eq!(
            valued.arms[1].values,
            [Value {
                quantity: "TTFT p50".into(),
                point: Some(1.0),
                rep: 1,
                value: 0.93
            }]
        );
    }

    #[test]
    fn values_for_an_arm_the_runs_do_not_have_are_refused_naming_those_they_do() {
        let measured = measurement_of(&[run(COMPARISON)], "20261027-1").unwrap();

        let refused = with_values(measured, r#"{"main": []}"#);

        assert_eq!(
            refused.unwrap_err().to_string(),
            "--values names the arm main, and the runs have base, local"
        );
    }

    #[test]
    fn the_measurement_is_written_as_json_with_its_fields_in_order() {
        let measured = measurement_of(&[run(&single("1", "a1", 1793116800))], "1").unwrap();

        assert_eq!(
            serde_json::to_string(&measured).unwrap(),
            r#"{"measured_at":"2026-10-27T16:00:00Z","procedure":"cargo bench --bench serve","conditions":{"label":"app/bench/serve","machine":"bench-box"},"source":"dibs jobs 1","arms":[{"name":"app@a1","revisions":{"app":"a1"},"values":[]}]}"#
        );
    }
}
