use super::{jobs::Jobs, refs::Arm};
use crate::recipe::Lock;
use dibs_format::{BatchId, JobId, StepRecord};
use std::{collections::BTreeMap, path::PathBuf};

/// Every job that kept files has them fetched, into `to` when given: under the job's arm and rep
/// when a comparison or reps would otherwise write one path twice.
pub(crate) fn fetch_artifacts(
    backend: &Jobs,
    steps: &[StepRecord],
    to: Option<&str>,
    compared: bool,
) {
    for s in steps.iter().filter(|s| s.artifacts.is_some_and(|n| n > 0)) {
        let Some(job) = &s.job else { continue };
        let dest = to.map(|dir| {
            let mut dest = PathBuf::from(dir);
            if let Some(arm) = s.arm.as_deref().filter(|_| compared) {
                dest.push(arm);
            }
            if let Some(r) = s.rep {
                dest.push(format!("r{r}"));
            }
            dest.display().to_string()
        });
        // Its report goes where every other dibs: line does, apart from the jobs' own output.
        match backend.fetch(job, dest.as_deref()) {
            Ok(report) => eprint!("{report}"),
            Err(exit) => eprintln!(
                "dibs: could not fetch what job {job} kept (exit {exit}); dibs --fetch {job} tries again"
            ),
        }
    }
}

/// Each arm's measured seconds per rep, and the jobs whose logs hold its numbers. The seconds
/// are the steps' wall time, which is only a first look: the recipe's own output is the result.
pub(crate) fn measured_summary(
    arms: &[Arm],
    steps: &[StepRecord],
    revisions: &dyn Fn(usize) -> Vec<(String, String)>,
) -> String {
    let width = arms.iter().map(|a| a.name.len()).max().unwrap_or(0);
    let mut out = String::from(
        "dibs: measured, each rep's exclusive seconds and the jobs with its output:\n",
    );
    for (a, arm) in arms.iter().enumerate() {
        let mine: Vec<&StepRecord> = steps
            .iter()
            .filter(|s| {
                s.lock == Lock::Exclusive
                    && (arms.len() == 1 || s.arm.as_deref() == Some(arm.name.as_str()))
            })
            .collect();
        let mut per_rep: BTreeMap<u32, u64> = BTreeMap::new();
        for s in &mine {
            *per_rep.entry(s.rep.unwrap_or(1)).or_default() += s.seconds;
        }
        let secs: Vec<String> = per_rep.values().map(|s| format!("{s}s")).collect();
        let jobs: Vec<&str> = mine
            .iter()
            .filter_map(|s| s.job.as_ref().map(JobId::as_str))
            .collect();
        let revs: Vec<String> = revisions(a)
            .iter()
            .map(|(r, sha)| format!("{r}@{sha}"))
            .collect();
        out += &format!(
            "  {:<width$}  {}  {}  jobs {}\n",
            arm.name,
            revs.join(" "),
            if secs.is_empty() {
                "nothing measured".to_string()
            } else {
                secs.join(" ")
            },
            if jobs.is_empty() {
                "-".to_string()
            } else {
                jobs.join(" ")
            }
        );
    }
    out
}

/// The batch a call is a step of, as the batch driver told it.
pub(crate) fn batch_of_caller() -> Option<BatchId> {
    std::env::var("DIBS_BATCH")
        .ok()
        .filter(|b| !b.is_empty())
        .map(BatchId::from)
}
