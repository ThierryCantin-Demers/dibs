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

/// Under the scratch, a directory per attempt: its `receipt` names the job and is locked by the
/// runner that serves it, `started` says the job held the machine's lock, and `trailer.json` is
/// how it ended. None of it is swept with the job's log, so an attempt is answered for as long
/// as it is asked about.
pub struct Attempts {
    dir: PathBuf,
}

/// An admitted attempt's receipt, held locked for as long as this runner serves it.
#[derive(Debug)]
pub struct Receipt {
    _held: File,
}

const RECEIPT: &str = "receipt";
const STARTED: &str = "started";
const TRAILER: &str = "trailer.json";

impl Attempts {
    pub const DIR: &'static str = "attempts";

    pub fn under(scratch: &Path) -> Attempts {
        Attempts {
            dir: scratch.join(Attempts::DIR),
        }
    }

    /// The attempt admitted for `job`, or how the attempt admitted before has gone. An id that
    /// is not one path component is refused.
    pub fn admit(&self, id: &str, job: &JobId) -> io::Result<Result<Receipt, Admitted>> {
        let attempt = self.dir.join(Attempts::checked(id)?);
        fs::create_dir_all(&self.dir)?;
        // Published whole by a rename, so whoever finds it finds its job named and locked.
        let part = self
            .dir
            .join(format!(".part-{}-{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&part);
        fs::create_dir(&part)?;
        let mut file = File::create(part.join(RECEIPT))?;
        file.lock()?;
        file.write_all(format!("{job}\n").as_bytes())?;
        match fs::rename(&part, &attempt) {
            Ok(()) => Ok(Ok(Receipt { _held: file })),
            Err(_) if attempt.join(RECEIPT).exists() => {
                drop(file);
                let _ = fs::remove_dir_all(&part);
                Ok(Err(Attempts::seen(&attempt)?))
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&part);
                Err(e)
            }
        }
    }

    /// The job of attempt `id` holds the machine's lock now: from here it may have run.
    pub fn started(&self, id: &str) {
        if let Ok(id) = Attempts::checked(id) {
            let _ = File::create(self.dir.join(id).join(STARTED));
        }
    }

    /// How attempt `id` ended, kept before its caller is told.
    pub fn ended(&self, id: &str, trailer: &Trailer) {
        let (Ok(id), Ok(text)) = (Attempts::checked(id), serde_json::to_vec(trailer)) else {
            return;
        };
        let dir = self.dir.join(id);
        let part = dir.join(format!("{TRAILER}.part"));
        if fs::write(&part, text).is_ok() {
            let _ = fs::rename(&part, dir.join(TRAILER));
        }
    }

    fn checked(id: &str) -> io::Result<&str> {
        let valid = !id.is_empty()
            && id.len() <= 200
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && !id.starts_with('.');
        match valid {
            true => Ok(id),
            false => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{id:?} is not an attempt id: letters, digits, dots, dashes and underscores"
                ),
            )),
        }
    }

    /// How the attempt at `dir` went: its runner still holds it, or it left a trailer, or not.
    fn seen(dir: &Path) -> io::Result<Admitted> {
        let receipt = dir.join(RECEIPT);
        let file = File::open(&receipt)?;
        let job = JobId::new(fs::read_to_string(&receipt)?.trim());
        if let Err(fs::TryLockError::WouldBlock) = file.try_lock_shared() {
            let pid = Attempts::holder(&job);
            return Ok(Admitted::Running { job, pid });
        }
        match fs::read(dir.join(TRAILER))
            .ok()
            .and_then(|text| serde_json::from_slice::<Trailer>(&text).ok())
        {
            Some(trailer) => Ok(Admitted::Ended(trailer)),
            None => Ok(Admitted::Lost {
                started: dir.join(STARTED).exists(),
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
        let job = JobId::new("20261124-120000-4242");
        let receipt = attempts.admit("sub-1.build.1", &job).unwrap().unwrap();
        let again = attempts.admit("sub-1.build.1", &job).unwrap();
        assert_eq!(
            again.unwrap_err(),
            Admitted::Running {
                job: job.clone(),
                pid: 4242
            }
        );
        drop(receipt);
        assert_eq!(
            attempts.admit("sub-1.build.1", &job).unwrap().unwrap_err(),
            Admitted::Lost {
                job: job.clone(),
                started: false
            },
            "gone before it held the lock, it never ran"
        );
        attempts.started("sub-1.build.1");
        assert!(matches!(
            attempts.admit("sub-1.build.1", &job).unwrap(),
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
        attempts.ended("sub-1.build.1", &trailer);
        assert_eq!(
            attempts.admit("sub-1.build.1", &job).unwrap().unwrap_err(),
            Admitted::Ended(trailer)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_id_that_names_a_path_is_refused() {
        let dir = scratch("path");
        let attempts = Attempts::under(&dir);
        for id in ["../x", ".hidden", "", "a/b"] {
            assert!(attempts.admit(id, &JobId::new("1-2")).is_err(), "{id:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
