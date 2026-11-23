//! A recipe run whose last job checked in, kept beside that job: once a follower has seen the
//! job end, it fills in the last step, fetches what the job kept and records the run.

use super::{
    error::RunError,
    jobs::{Jobs, Reader, Reported},
    kept::KeptStores,
};
use crate::{
    call::{MachineCall, Output},
    caller::Caller,
    cli::Call,
    paths::Paths,
    records::RunLog,
};
use dibs_format::{JobId, Moment, Outcome, RunRecord, wire::Prepared};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Unfinished {
    /// Its last step stands for the job that checked in, until a follower sees it end.
    pub record: RunRecord,
    pub artifacts_to: Option<String>,
    pub copies: BTreeMap<String, String>,
}

impl Unfinished {
    fn path(kept_jobs: &Path, job: &JobId) -> PathBuf {
        kept_jobs.join(job.as_str()).join("run")
    }

    /// Where a follow of `job` finds the run it ends, when there is one.
    pub fn of(job: &JobId) -> Option<PathBuf> {
        let path = Unfinished::path(&Paths::from_env().kept_jobs()?, job);
        path.is_file().then_some(path)
    }

    pub fn keep(&self, job: &JobId) {
        let kept = Paths::from_env()
            .kept_jobs()
            .map(|kept| Unfinished::path(&kept, job));
        let written = kept.as_ref().is_some_and(|path| {
            path.parent()
                .is_some_and(|dir| fs::create_dir_all(dir).is_ok())
                && serde_json::to_vec(self).is_ok_and(|json| fs::write(path, json).is_ok())
        });
        match written {
            true => eprintln!(
                "  the run is recorded, and what the job keeps fetched, by the dibs --follow that sees it end"
            ),
            false => eprintln!(
                "dibs: this run could not be kept for its follower, so it will not be recorded"
            ),
        }
    }

    /// Follows the job to its end, then finishes the run kept at `path`: whoever claims it by a
    /// rename does, so two followers record it once.
    pub fn follow(path: &Path, job: &JobId, call: &Call, caller: &Caller) -> Result<i32, RunError> {
        let machine = MachineCall::new(call, caller)?;
        let mut on_prepared = |_: &Prepared| {};
        let mut reader = Reader::new(&mut on_prepared);
        let exit = Jobs::exit(machine.follow_into(job, &mut Output::Listening(&mut reader)));
        let reported = reader.reported(exit);
        // Only the job's own trailer says it ended: a follow that never reached it leaves the
        // run for the next.
        let ended = reported
            .outcome
            .trailer
            .as_ref()
            .is_some_and(|t| t.job == job.as_str());
        if reported.outcome.checked_in.is_some() || !ended {
            return Ok(exit);
        }
        let claimed = path.with_extension(std::process::id().to_string());
        if fs::rename(path, &claimed).is_err() {
            return Ok(exit);
        }
        let unfinished = fs::read(&claimed)
            .ok()
            .and_then(|json| serde_json::from_slice::<Unfinished>(&json).ok());
        let Some(unfinished) = unfinished else {
            let _ = fs::remove_file(&claimed);
            eprintln!(
                "dibs: the run job {job} ended could not be read back, so it is not recorded"
            );
            return Ok(exit);
        };
        match unfinished.finish(reported) {
            Ok(()) => {
                let _ = fs::remove_file(&claimed);
                Ok(exit)
            }
            Err(e) => {
                let _ = fs::rename(&claimed, path);
                Err(e)
            }
        }
    }

    fn finish(self, reported: Reported) -> Result<(), RunError> {
        let Unfinished {
            mut record,
            artifacts_to,
            copies,
        } = self;
        let Some(last) = record.steps.last_mut() else {
            return Ok(());
        };
        let stepped = reported.stepped.as_ref();
        *last = dibs_format::StepRecord {
            arm: last.arm.take(),
            rep: last.rep,
            artifacts: stepped.and_then(|s| s.artifacts),
            ..reported.outcome.step_record(last.lock)
        };
        if let Some(state) = stepped
            .and_then(|s| s.state.clone())
            .filter(|state| !state.is_empty())
        {
            record.state = state;
        }
        record.outcome = Some(Outcome::of_steps(&record.steps));
        record.when = Moment::epoch_now();
        let backend = Jobs::on(record.machine.clone());
        let at = record.steps.len() - 1;
        backend.fetch_artifacts(&record.steps[at..], artifacts_to.as_deref(), false);
        if let Some(kept) = KeptStores::of(record.machine.clone(), &copies) {
            kept.fetch();
        }
        RunLog::here()?.append(&record)?;
        Ok(())
    }
}
