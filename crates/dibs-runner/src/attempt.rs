//! A caller's attempt at a piece of work, admitted once per machine: a request naming one seen
//! before is told how that one went and never runs again, so a driver that lost its call can
//! ask without risking the work twice.

use dibs_format::{
    JobId,
    wire::{Admission, Trailer},
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
    pub fn admit(&self, id: &str, job: &JobId) -> io::Result<Result<Receipt, Admission>> {
        self.publish(id, Some(job))
    }

    /// How attempt `id` went, without admitting it. One never admitted here is closed instead,
    /// so a request for it still on its way finds it ended and never runs: `Never`.
    pub fn look(&self, id: &str) -> io::Result<Admission> {
        match self.publish(id, None)? {
            Ok(_) => Ok(Admission::Never),
            Err(admitted) => Ok(admitted),
        }
    }

    /// Publishes attempt `id`'s directory whole by a rename, so whoever finds it finds its
    /// receipt already locked and naming its job; with no job, a closed one that nothing holds.
    /// Or how the one there went.
    fn publish(&self, id: &str, job: Option<&JobId>) -> io::Result<Result<Receipt, Admission>> {
        let attempt = self.dir.join(Attempts::checked(id)?);
        fs::create_dir_all(&self.dir)?;
        let part = self
            .dir
            .join(format!(".part-{}-{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&part);
        fs::create_dir(&part)?;
        let mut file = File::create(part.join(RECEIPT))?;
        file.lock()?;
        if let Some(job) = job {
            file.write_all(format!("{job}\n").as_bytes())?;
        }
        let published = fs::rename(&part, &attempt);
        if published.is_err() {
            let _ = fs::remove_dir_all(&part);
        }
        match published {
            Ok(()) => Ok(Ok(Receipt { _held: file })),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                drop(file);
                Ok(Err(Attempts::seen(&attempt)?))
            }
            Err(e) => Err(e),
        }
    }

    /// The job of attempt `id` holds the machine's lock now: from here it may have run, so it
    /// does not run unless this is kept.
    pub fn started(&self, id: &str) -> io::Result<()> {
        File::create(self.dir.join(Attempts::checked(id)?).join(STARTED)).map(drop)
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
    fn seen(dir: &Path) -> io::Result<Admission> {
        let receipt = dir.join(RECEIPT);
        let file = File::open(&receipt)?;
        let named = fs::read_to_string(&receipt)?;
        if named.trim().is_empty() {
            return Ok(Admission::Never);
        }
        let job = JobId::new(named.trim());
        if let Err(fs::TryLockError::WouldBlock) = file.try_lock_shared() {
            let pid = Attempts::holder(&job);
            return Ok(Admission::Running { job, pid });
        }
        match fs::read(dir.join(TRAILER))
            .ok()
            .and_then(|text| serde_json::from_slice::<Trailer>(&text).ok())
        {
            Some(trailer) => Ok(Admission::Ended(trailer)),
            None => Ok(Admission::Lost {
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
    use dibs_format::{ExitSource, Label, Mode};

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
            Admission::Running {
                job: job.clone(),
                pid: 4242
            }
        );
        drop(receipt);
        assert_eq!(
            attempts.admit("sub-1.build.1", &job).unwrap().unwrap_err(),
            Admission::Lost {
                job: job.clone(),
                started: false
            },
            "gone before it held the lock, it never ran"
        );
        attempts.started("sub-1.build.1").unwrap();
        assert!(matches!(
            attempts.admit("sub-1.build.1", &job).unwrap(),
            Err(Admission::Lost { started: true, .. })
        ));
        let trailer = Trailer {
            job: job.clone(),
            mode: Mode::Shared,
            label: Label::new("build"),
            queued_secs: 0,
            ran_secs: 2,
            exit: 0,
            by: ExitSource::Command,
            built: None,
            test_count: None,
        };
        attempts.ended("sub-1.build.1", &trailer);
        assert_eq!(
            attempts.admit("sub-1.build.1", &job).unwrap().unwrap_err(),
            Admission::Ended(trailer)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_attempt_asked_after_before_it_arrives_never_runs() {
        let dir = scratch("looked");
        let attempts = Attempts::under(&dir);
        assert_eq!(attempts.look("sub-1.measure.1").unwrap(), Admission::Never);
        assert_eq!(
            attempts
                .admit("sub-1.measure.1", &JobId::new("1-2"))
                .unwrap()
                .unwrap_err(),
            Admission::Never
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
