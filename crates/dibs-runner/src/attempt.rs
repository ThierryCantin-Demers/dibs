//! A caller's attempt at a piece of work, admitted once per machine: a request naming one seen
//! before is told how that one went and never runs again, so a driver that lost its call can
//! ask without risking the work twice.

use dibs_format::{
    JobId,
    wire::{Admitted, Trailer},
};
use std::{
    fs::{self, File},
    io::{self, Write as _},
    path::{Path, PathBuf},
};

/// Under the scratch: one file per attempt naming its job, locked by the runner that serves it.
pub struct Attempts {
    dir: PathBuf,
}

/// An admitted attempt's file, held locked for as long as this runner serves it.
#[derive(Debug)]
pub struct Receipt {
    _held: File,
}

/// Beside a job's log: its trailer, written before the caller is told.
pub const TRAILER: &str = "trailer.json";

impl Attempts {
    pub const DIR: &'static str = "attempts";

    pub fn under(scratch: &Path) -> Attempts {
        Attempts {
            dir: scratch.join(Attempts::DIR),
        }
    }

    /// The attempt admitted for `job`, or how the attempt admitted before has gone. An id that
    /// is not one path component is refused as an attempt never admitted and lost.
    pub fn admit(
        &self,
        id: &str,
        job: &JobId,
        jobs: &Path,
    ) -> io::Result<Result<Receipt, Admitted>> {
        let valid = !id.is_empty()
            && id.len() <= 200
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && !id.starts_with('.');
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{id:?} is not an attempt id: letters, digits, dots, dashes and underscores"
                ),
            ));
        }
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(id);
        match File::options().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.lock()?;
                file.write_all(format!("{job}\n").as_bytes())?;
                Ok(Ok(Receipt { _held: file }))
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(Err(self.seen(&path, jobs)?)),
            Err(e) => Err(e),
        }
    }

    /// How the attempt at `path` went: its runner still holds it, or it left a trailer, or not.
    fn seen(&self, path: &Path, jobs: &Path) -> io::Result<Admitted> {
        let file = File::open(path)?;
        let named = fs::read_to_string(path)?;
        let job = JobId::new(named.trim());
        if let Err(fs::TryLockError::WouldBlock) = file.try_lock_shared() {
            let pid = Attempts::holder(&job);
            return Ok(Admitted::Running { job, pid });
        }
        let dir = jobs.join(job.as_str());
        match fs::read(dir.join(TRAILER))
            .ok()
            .and_then(|text| serde_json::from_slice::<Trailer>(&text).ok())
        {
            Some(trailer) => Ok(Admitted::Ended(trailer)),
            None => Ok(Admitted::Lost {
                started: dir.join("cmd").exists(),
                job,
            }),
        }
    }

    /// The runner a job id names: the pid after its start time.
    fn holder(job: &JobId) -> u32 {
        job.as_str()
            .rsplit('-')
            .next()
            .and_then(|pid| pid.parse().ok())
            .unwrap_or(0)
    }
}

/// A job's trailer, kept beside its log before the caller hears it.
pub fn keep_trailer(job_dir: &Path, trailer: &Trailer) {
    let Ok(text) = serde_json::to_vec(trailer) else {
        return;
    };
    let part = job_dir.join(format!("{TRAILER}.part"));
    if fs::write(&part, text).is_ok() {
        let _ = fs::rename(&part, job_dir.join(TRAILER));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dibs_format::{By, Label, Mode};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dibs-attempts-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("jobs")).unwrap();
        dir
    }

    #[test]
    fn an_attempt_is_admitted_once_and_then_told_how_it_went() {
        let dir = scratch("once");
        let attempts = Attempts::under(&dir);
        let jobs = dir.join("jobs");
        let job = JobId::new("20261124-120000-4242");
        let receipt = attempts
            .admit("sub-1.build.1", &job, &jobs)
            .unwrap()
            .unwrap();
        let again = attempts.admit("sub-1.build.1", &job, &jobs).unwrap();
        assert_eq!(
            again.unwrap_err(),
            Admitted::Running {
                job: job.clone(),
                pid: 4242
            }
        );
        drop(receipt);
        assert_eq!(
            attempts
                .admit("sub-1.build.1", &job, &jobs)
                .unwrap()
                .unwrap_err(),
            Admitted::Lost {
                job: job.clone(),
                started: false
            },
            "gone before it held the lock, it never ran"
        );
        let job_dir = jobs.join(job.as_str());
        fs::create_dir_all(&job_dir).unwrap();
        fs::write(job_dir.join("cmd"), "true").unwrap();
        assert!(matches!(
            attempts.admit("sub-1.build.1", &job, &jobs).unwrap(),
            Err(Admitted::Lost { started: true, .. })
        ));
        let trailer = Trailer {
            job: job.clone(),
            mode: Mode::Shared,
            label: Label::new("build"),
            queued: 0,
            ran: 2,
            exit: 0,
            by: By::Command,
            built: None,
            tests: None,
        };
        keep_trailer(&job_dir, &trailer);
        assert_eq!(
            attempts
                .admit("sub-1.build.1", &job, &jobs)
                .unwrap()
                .unwrap_err(),
            Admitted::Ended(trailer)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_id_that_names_a_path_is_refused() {
        let dir = scratch("path");
        let attempts = Attempts::under(&dir);
        for id in ["../x", ".hidden", "", "a/b"] {
            assert!(
                attempts
                    .admit(id, &JobId::new("1-2"), &dir.join("jobs"))
                    .is_err(),
                "{id:?}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
