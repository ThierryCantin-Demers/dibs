use super::parse::{Step, StepKind};
use crate::call::BatchStep;
use dibs_format::Mode;
use std::fmt;

/// A step not yet started, as the machine's `--status` reads it to say how long a batch has left.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub name: String,
    pub mode: Planned,
    pub label: String,
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

/// What a step of a batch is started with. `DIBS_BATCH_PLAN` is `k<TAB>n` and then one pending
/// step per line; dibs sends it with the job and the machine keeps it beside the holder.
pub fn step_env(id: &str, step: &str, k: usize, n: usize, pending: &[Pending]) -> BatchStep {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut plan = format!("{k}\t{n}\n");
    for p in pending {
        plan.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            clean(&p.name),
            p.mode,
            history_key(&p.label),
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
pub fn recipe_env(own_id: &str, calls: &[Pending], k: usize) -> Option<BatchStep> {
    nested_env(BatchStep::from_env(), own_id, calls, k)
}

pub fn nested_env(
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
            let mut env = step_env(&outer.batch, &step, bk, bn, pending);
            env.plan.push_str(rest);
            Some(env)
        }
        None if calls.len() > 1 => Some(step_env(
            own_id,
            &calls[k].name,
            k + 1,
            calls.len(),
            pending,
        )),
        None => None,
    }
}

/// The history key dibs will file a step under, which is what its estimate is looked up by.
pub fn pending_of(step: &Step, here: bool, cwd: &str) -> Pending {
    let directory = || cwd.rsplit('/').next().unwrap_or_default().to_string();
    let (mode, default) = match step.lock {
        StepKind::Sync => (Planned::Job(Mode::Rsh), "sync".to_string()),
        StepKind::Recipe => (Planned::Recipe, String::new()),
        StepKind::Shared => (Planned::Job(Mode::Shared), directory()),
        StepKind::Bench => (Planned::Job(Mode::Bench), directory()),
        StepKind::Peek => (Planned::Job(Mode::Peek), directory()),
    };
    Pending {
        name: step.name.clone(),
        mode,
        label: step.label.clone().unwrap_or(default),
        here,
    }
}

/// dibs files a label with everything but `[A-Za-z0-9._-]` replaced.
pub fn history_key(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn plan(steps: &[Step], machines: &[String]) -> String {
    let w = steps.iter().map(|s| s.name.len()).max().unwrap_or(4).max(4);
    let m = machines.iter().map(String::len).max().unwrap_or(7).max(7);
    let mut s = String::new();
    for (i, st) in steps.iter().enumerate() {
        let after = if st.after.is_empty() {
            "-".to_string()
        } else {
            st.after.join(",")
        };
        s.push_str(&format!(
            "  {:w$}  {:m$}  {:6}  after {}{}\n",
            st.name,
            machines[i],
            st.lock,
            after,
            if st.cont { ", cont" } else { "" }
        ));
    }
    s
}
