use crate::{
    BatchId, Label, Mode,
    lines::{error::LineError, field::Field, reader::FieldReader},
    status::Within,
};
use std::{fmt, str::FromStr};

/// What a batch step still to come will run as.
///
/// # Guarantees
///
/// - `from_str` reads back every name `Display` writes, and refuses any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PendingStepKind {
    Job(Mode),
    /// A recipe, which runs several jobs under labels of its own and so has no single history.
    Recipe,
}

impl PendingStepKind {
    fn as_str(self) -> &'static str {
        match self {
            PendingStepKind::Job(mode) => mode.as_str(),
            PendingStepKind::Recipe => "recipe",
        }
    }
}

impl fmt::Display for PendingStepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PendingStepKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "recipe" => Ok(PendingStepKind::Recipe),
            mode => mode.parse().map(PendingStepKind::Job),
        }
    }
}

/// A batch step still to come, which a machine estimates the batch's time left from: one line of
/// a [`BatchPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingStep {
    pub name: String,
    pub kind: PendingStepKind,
    /// The label as the machine files it, empty for a recipe.
    pub label: Label,
    /// On the same machine as the step carrying the plan.
    pub here: bool,
}

/// The plan a batch step carries and a machine keeps beside its holder record, as `batch.<pid>`:
/// which batch, which step of how many, and the steps still to come.
///
/// # Guarantees
///
/// - Writes the plan a machine keeps today, byte for byte, with 4 fields to each step still to
///   come, and reads it with blank lines after its count skipped.
/// - A count line of 2 or 3 fields reads with `within` none.
/// - Free text never adds a field or a line: a tab, a newline or a carriage return in it is
///   written as a space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPlan {
    pub batch: BatchId,
    /// The step's name, prefixed by the outer step's when a recipe inside a batch carries it.
    pub step: String,
    /// The step's number, counting from 1.
    pub position: usize,
    pub step_count: usize,
    pub within: Option<Within>,
    /// The steps still to come, read in the order they run.
    pub pending: Vec<PendingStep>,
}

impl BatchPlan {
    /// The text of `batch.<pid>`, each line ending in a newline.
    pub fn to_text(&self) -> String {
        let within = self
            .within
            .map(|within| format!("\t{}\t{}", within.job, within.jobs))
            .unwrap_or_default();
        let pending: String = self
            .pending
            .iter()
            .map(|step| format!("{}\n", step_line(step)))
            .collect();
        format!(
            "{}\t{}\n{}\t{}{within}\n{pending}",
            Field::from_text(self.batch.as_str()),
            Field::from_text(&self.step),
            self.position,
            self.step_count,
        )
    }
}

impl FromStr for BatchPlan {
    type Err = LineError;

    /// # Errors
    ///
    /// - [`LineError::Missing`] for a plan without its batch line or its count line.
    /// - [`LineError::Value`] for a position, a total, a job or a jobs that does not read, and
    ///   for a step still to come whose kind does not read or whose `here` is neither `0` nor
    ///   `1`.
    /// - [`LineError::FieldCount`] for a step still to come of other than 4 fields.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut lines = text.lines();
        let mut next_line = |field: &'static str| {
            lines
                .next()
                .map(FieldReader::of_line)
                .ok_or(LineError::Missing { field })
        };
        let mut head = next_line("batch")?;
        let mut count = next_line("position")?;
        Ok(BatchPlan {
            batch: BatchId::new(head.read_text()),
            step: head.read_text().to_string(),
            position: count.read_parsed("position")?,
            step_count: count.read_parsed("total")?,
            within: match count.count() {
                4.. => Some(Within {
                    job: count.read_parsed("job")?,
                    jobs: count.read_parsed("jobs")?,
                }),
                _ => None,
            },
            pending: lines
                .filter(|line| !line.is_empty())
                .map(read_step)
                .collect::<Result<_, _>>()?,
        })
    }
}

fn step_line(step: &PendingStep) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        Field::from_text(&step.name),
        step.kind,
        Field::from_text(step.label.as_str()),
        u8::from(step.here)
    )
}

fn read_step(line: &str) -> Result<PendingStep, LineError> {
    let mut f = FieldReader::of_record(line, "pending step", 4..=4)?;
    Ok(PendingStep {
        name: f.read_text().to_string(),
        kind: f.read_parsed("kind")?,
        label: Label::new(f.read_text()),
        here: parse_flag("here", f.read_text())?,
    })
}

/// A field that is `1` for true and `0` for false.
fn parse_flag(field: &'static str, text: &str) -> Result<bool, LineError> {
    match text {
        "1" => Ok(true),
        "0" => Ok(false),
        other => Err(LineError::Value {
            field,
            value: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::reader::line_with_field;

    const SWEEP: &str = "b1\tsweep: a\n1\t3\nb\trecipe\t\t1\nc\tpeek\tps\t0\n";
    const HEAD: &str = "b1\tstep\n1\t2\n";
    const STEP: &str = "next\tshared\trec-next\t1";

    #[test]
    fn the_plans_a_machine_keeps_read_and_write_back_byte_for_byte() {
        for text in [
            "20261001-120000-77\tapp/bench/held@cpu\n2\t2\n",
            "20261001-120000-78\tserve\n1\t2\nnext\tshared\trec-next\t1\n",
            "20261001-120000-79\tsweep: a\n1\t3\nb\trecipe\t\t1\nc\tbench\tc-bench\t0\n",
            "20261001-120000-80\tab: app/bench/x (b r2)\n1\t11\t17\t20\nnext\tbench\tapp_bench_x\t1\n",
        ] {
            assert_eq!(text.parse::<BatchPlan>().unwrap().to_text(), text);
        }
    }

    #[test]
    fn a_plan_names_its_step_of_how_many() {
        let plan: BatchPlan = SWEEP.parse().unwrap();
        assert_eq!((plan.position, plan.step_count, plan.within), (1, 3, None));
    }

    #[test]
    fn a_plan_names_each_step_still_to_come() {
        let plan: BatchPlan = SWEEP.parse().unwrap();
        assert_eq!(
            plan.pending,
            [
                PendingStep {
                    name: "b".into(),
                    kind: PendingStepKind::Recipe,
                    label: Label::new(""),
                    here: true,
                },
                PendingStep {
                    name: "c".into(),
                    kind: PendingStepKind::Job(Mode::Peek),
                    label: Label::new("ps"),
                    here: false,
                },
            ]
        );
    }

    #[test]
    fn blank_lines_after_the_count_are_skipped() {
        let spaced: BatchPlan = "b1\tsweep: a\n1\t3\n\nb\trecipe\t\t1\n\nc\tpeek\tps\t0\n"
            .parse()
            .unwrap();
        assert_eq!(spaced, SWEEP.parse().unwrap());
    }

    #[test]
    fn an_empty_plan_is_refused() {
        assert_eq!(
            "".parse::<BatchPlan>(),
            Err(LineError::Missing { field: "batch" })
        );
    }

    #[test]
    fn a_plan_without_its_count_is_refused() {
        assert_eq!(
            "b1\tstep\n".parse::<BatchPlan>(),
            Err(LineError::Missing { field: "position" })
        );
    }

    #[test]
    fn each_count_that_does_not_read_is_refused_by_name() {
        for (index, field) in [(0, "position"), (1, "total"), (2, "job"), (3, "jobs")] {
            let count = line_with_field("1\t11\t17\t20", index, "?");
            assert_eq!(
                format!("b1\tstep\n{count}\n").parse::<BatchPlan>(),
                Err(LineError::Value {
                    field,
                    value: "?".into()
                })
            );
        }
    }

    #[test]
    fn a_step_of_other_than_four_fields_is_refused() {
        assert_eq!(
            format!("{HEAD}next\tshared\trec-next\n").parse::<BatchPlan>(),
            Err(LineError::FieldCount {
                record: "pending step",
                found: 3
            })
        );
    }

    #[test]
    fn each_step_field_that_does_not_read_is_refused_by_name() {
        for (index, field) in [(1, "kind"), (3, "here")] {
            let step = line_with_field(STEP, index, "?");
            assert_eq!(
                format!("{HEAD}{step}\n").parse::<BatchPlan>(),
                Err(LineError::Value {
                    field,
                    value: "?".into()
                })
            );
        }
    }

    #[test]
    fn a_kind_reads_back_from_its_name() {
        for kind in [PendingStepKind::Recipe, PendingStepKind::Job(Mode::Bench)] {
            assert_eq!(kind.to_string().parse::<PendingStepKind>(), Ok(kind));
        }
    }

    #[test]
    fn a_name_no_kind_has_is_refused() {
        assert!("sprint".parse::<PendingStepKind>().is_err());
    }
}
