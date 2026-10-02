use super::parse::Step;
use dibs::call::BatchStep;

/// A step not yet started, as the machine's `--status` reads it to say how long a batch has left.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub name: String,
    /// The lock as the duration history files it: shared, bench, rsh, peek, or recipe, which
    /// runs several jobs under labels of its own and so has no single history.
    pub mode: &'static str,
    pub label: String,
    /// On the same machine as the step carrying the plan.
    pub here: bool,
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

pub(crate) fn nested_env(
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
pub(crate) fn pending_of(step: &Step, here: bool, cwd: &str) -> Pending {
    let (mode, default) = match step.lock {
        "sync" => ("rsh", "sync".to_string()),
        "recipe" => ("recipe", String::new()),
        other => (
            other,
            cwd.rsplit('/').next().unwrap_or_default().to_string(),
        ),
    };
    Pending {
        name: step.name.clone(),
        mode,
        label: step.label.clone().unwrap_or(default),
        here,
    }
}

/// dibs files a label with everything but `[A-Za-z0-9._-]` replaced.
pub(crate) fn history_key(label: &str) -> String {
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

pub(crate) fn plan(steps: &[Step], machines: &[String]) -> String {
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
