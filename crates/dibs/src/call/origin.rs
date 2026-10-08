use dibs_format::{Label, Mode, status::Within, wire::Tree};
use std::fmt;

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
    /// The tree it runs in, prepared at its head when it is not yet.
    pub tree: Option<Tree>,
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

    /// What a step of a batch is started with. `DIBS_BATCH_PLAN` is `k<TAB>n` and then one pending
    /// step per line; dibs sends it with the job and the machine keeps it beside the holder.
    pub fn new(
        id: &str,
        step: &str,
        k: usize,
        n: usize,
        within: Option<Within>,
        pending: &[Pending],
    ) -> BatchStep {
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        let mut plan = match within {
            Some(w) => format!("{k}\t{n}\t{}\t{}\n", w.job, w.jobs),
            None => format!("{k}\t{n}\n"),
        };
        for p in pending {
            plan.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                clean(&p.name),
                p.mode,
                p.label.filed(),
                u8::from(p.here)
            ));
        }
        BatchStep {
            batch: clean(id),
            step: clean(step),
            plan,
        }
    }

    /// The plan each of a recipe's jobs carries. Inside a batch the recipe is one of its steps, so
    /// the recipe's jobs still to come go ahead of the batch's own.
    pub fn for_recipe_job(own_id: &str, calls: &[Pending], k: usize) -> Option<BatchStep> {
        BatchStep::for_recipe_job_in(BatchStep::from_env(), own_id, calls, k)
    }

    /// As `for_recipe_job`, inside `outer` rather than the batch this process was started in.
    pub fn for_recipe_job_in(
        outer: Option<BatchStep>,
        own_id: &str,
        calls: &[Pending],
        k: usize,
    ) -> Option<BatchStep> {
        let pending = &calls[k + 1..];
        match outer {
            Some(outer) => {
                let (head, rest) = outer
                    .plan
                    .split_once('\n')
                    .unwrap_or((outer.plan.as_str(), ""));
                let mut nums = head
                    .split('\t')
                    .map(|x| x.trim().parse::<usize>().unwrap_or(0));
                let (bk, bn) = (nums.next().unwrap_or(0), nums.next().unwrap_or(0));
                let step = format!("{}: {}", outer.step, calls[k].name);
                let within = (calls.len() > 1).then(|| Within {
                    job: k + 1,
                    jobs: calls.len(),
                });
                let mut env = BatchStep::new(&outer.batch, &step, bk, bn, within, pending);
                env.plan.push_str(rest);
                Some(env)
            }
            None if calls.len() > 1 => Some(BatchStep::new(
                own_id,
                &calls[k].name,
                k + 1,
                calls.len(),
                None,
                pending,
            )),
            None => None,
        }
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

/// A step not yet started, as the machine's `--status` reads it to say how long a batch has left.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub name: String,
    pub mode: Planned,
    pub label: Label,
    /// On the same machine as the step carrying the plan.
    pub here: bool,
}

/// What a pending step's duration is filed under: a mode, or a recipe, which runs several jobs
/// under labels of their own and so has no single history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Planned {
    Job(Mode),
    Recipe,
}

impl fmt::Display for Planned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Planned::Job(mode) => f.write_str(mode.as_str()),
            Planned::Recipe => f.write_str("recipe"),
        }
    }
}
