/// Who makes a call.
#[derive(Debug, Clone, Copy)]
pub enum Origin<'a> {
    /// A command line.
    Words,
    /// The recipe layer, which has placed its run already and said what a first run of a series
    /// means before building.
    Recipe(&'a RecipeJob),
}

/// What the recipe layer says about one of its jobs, which a command line leaves to the
/// environment its caller set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecipeJob {
    /// The batch the job is a step of, its recipe's own jobs ahead of the batch's.
    pub batch: Option<BatchStep>,
    /// The bound recipe's, which its duration is filed under beside the label.
    pub fingerprint: Option<String>,
}

/// Which batch a call is a step of and what is still to come, so the machine can say how long
/// the batch has left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchStep {
    pub batch: String,
    pub step: String,
    /// `k<TAB>n`, then one pending step per line.
    pub plan: String,
}

impl BatchStep {
    /// The most lines of a plan a call carries.
    const PLAN_LINES: usize = 200;

    /// The step this process's batch driver started it as.
    pub fn from_env() -> Option<BatchStep> {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        Some(var("DIBS_BATCH"))
            .filter(|batch| !batch.is_empty())
            .map(|batch| BatchStep {
                batch,
                step: var("DIBS_BATCH_STEP"),
                plan: var("DIBS_BATCH_PLAN"),
            })
    }

    /// What a step's shell is started with.
    pub fn vars(&self) -> [(&'static str, &str); 3] {
        [
            ("DIBS_BATCH", &self.batch),
            ("DIBS_BATCH_STEP", &self.step),
            ("DIBS_BATCH_PLAN", &self.plan),
        ]
    }

    /// As the machine keeps it beside the holder.
    pub fn sent(&self) -> String {
        let text = format!("{}\t{}\n{}\n", self.batch, self.step, self.plan);
        let kept: String = text
            .split_inclusive('\n')
            .take(BatchStep::PLAN_LINES)
            .collect();
        kept.trim_end_matches('\n').to_string()
    }
}
