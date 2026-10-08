use crate::{
    BatchId, Label, Mode,
    lines::base::{Field, Fields, LineError},
    status::Within,
};
use std::{fmt, str::FromStr};

/// What a batch step still to come will run as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PendingKind {
    Job(Mode),
    /// A recipe, which runs several jobs under labels of its own and so has no single history.
    Recipe,
}

impl fmt::Display for PendingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PendingKind::Job(mode) => mode.fmt(f),
            PendingKind::Recipe => f.write_str("recipe"),
        }
    }
}

impl FromStr for PendingKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "recipe" => Ok(PendingKind::Recipe),
            mode => mode.parse().map(PendingKind::Job),
        }
    }
}

/// A batch step still to come, which a machine estimates the batch's time left from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingStep {
    pub name: String,
    pub kind: PendingKind,
    /// The label as the machine files it, empty for a recipe.
    pub label: Label,
    /// On the same machine as the step carrying the plan.
    pub here: bool,
}

impl FromStr for PendingStep {
    type Err = LineError;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut f = Fields::of(line);
        if f.count() != 4 {
            return Err(LineError::FieldCount {
                record: "pending step",
                found: f.count(),
            });
        }
        Ok(PendingStep {
            name: f.text().to_string(),
            kind: f.parsed("kind")?,
            label: f.parsed("label")?,
            here: match f.text() {
                "1" => true,
                "0" => false,
                other => {
                    return Err(LineError::Value {
                        field: "here",
                        value: other.to_string(),
                    });
                }
            },
        })
    }
}

impl fmt::Display for PendingStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\t{}\t{}\t{}",
            Field(&self.name),
            self.kind,
            Field(self.label.as_str()),
            u8::from(self.here)
        )
    }
}

/// The plan a batch step carries and a machine keeps beside its holder record, as `batch.<pid>`:
/// which batch, which step of how many, and the steps still to come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPlan {
    pub batch: BatchId,
    /// The step's name, prefixed by the outer step's when a recipe inside a batch carries it.
    pub step: String,
    /// The step's number, counting from 1.
    pub position: usize,
    pub total: usize,
    pub within: Option<Within>,
    pub pending: Vec<PendingStep>,
}

impl FromStr for BatchPlan {
    type Err = LineError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut lines = text.lines();
        let mut head = Fields::of(lines.next().ok_or(LineError::Missing { field: "batch" })?);
        let mut count = Fields::of(
            lines
                .next()
                .ok_or(LineError::Missing { field: "position" })?,
        );
        Ok(BatchPlan {
            batch: head.parsed("batch")?,
            step: head.text().to_string(),
            position: count.parsed("position")?,
            total: count.parsed("total")?,
            within: match count.count() {
                4.. => Some(Within {
                    job: count.parsed("job")?,
                    jobs: count.parsed("jobs")?,
                }),
                _ => None,
            },
            pending: lines
                .filter(|line| !line.is_empty())
                .map(str::parse)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl fmt::Display for BatchPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}\t{}", Field(self.batch.as_str()), Field(&self.step))?;
        write!(f, "{}\t{}", self.position, self.total)?;
        if let Some(within) = self.within {
            write!(f, "\t{}\t{}", within.job, within.jobs)?;
        }
        writeln!(f)?;
        self.pending
            .iter()
            .try_for_each(|pending| writeln!(f, "{pending}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plans_a_machine_keeps_read_and_write_back_byte_for_byte() {
        for text in [
            "20261001-120000-77\tapp/bench/held@cpu\n2\t2\n",
            "20261001-120000-78\tserve\n1\t2\nnext\tshared\trec-next\t1\n",
            "20261001-120000-79\tsweep: a\n1\t3\nb\trecipe\t\t1\nc\tbench\tc-bench\t0\n",
            "20261001-120000-80\tab: app/bench/x (b r2)\n1\t11\t17\t20\nnext\tbench\tapp_bench_x\t1\n",
        ] {
            assert_eq!(text.parse::<BatchPlan>().unwrap().to_string(), text);
        }
    }

    #[test]
    fn a_plan_names_its_step_and_what_is_still_to_come() {
        let plan: BatchPlan = "b1\tsweep: a\n1\t3\nb\trecipe\t\t1\nc\tpeek\tps\t0\n"
            .parse()
            .unwrap();
        assert_eq!((plan.position, plan.total, plan.within), (1, 3, None));
        assert_eq!(plan.pending[0].kind, PendingKind::Recipe);
        assert_eq!(plan.pending[1].kind, PendingKind::Job(Mode::Peek));
        assert!(plan.pending[0].here && !plan.pending[1].here);
    }

    #[test]
    fn a_plan_without_its_count_is_refused() {
        assert_eq!(
            "b1\tstep\n".parse::<BatchPlan>(),
            Err(LineError::Missing { field: "position" })
        );
    }
}
