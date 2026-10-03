use crate::{
    call::{
        base::CallError,
        machine::{Asked, Bound, MachineCall},
    },
    cli::{Call, KillTarget},
    machine::Kept,
    render::Answers,
};
use dibs_format::{BatchId, Exit, Label, MachineName, Mode};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Write as _},
    path::Path,
    time::Duration,
};

/// How long a batch's driver has to stop on its own before it is sent SIGTERM.
const DRIVER_GRACE: Duration = Duration::from_secs(60);
/// How long each machine has to answer a kill of a batch driven from elsewhere.
const KILL_POLL_SECS: u64 = 20;

/// `dibs --kill`: a holder by its pid, or every step of a batch.
pub struct Kill<'a> {
    pub machine: &'a MachineCall<'a>,
    pub force: bool,
    pub anyone: bool,
}

impl Kill<'_> {
    pub fn answer(&self, target: &KillTarget) -> Result<i32, CallError> {
        match target {
            KillTarget::Pid(pid) => {
                let at = self.machine.target()?;
                self.machine.somewhere(&at)?;
                self.machine.send(self.asked(&pid.to_string()), &at)
            }
            KillTarget::Batch(batch) => self.batch(batch),
        }
    }

    fn asked(&self, target: &str) -> Asked {
        let label = match self.anyone {
            true => format!("{target}.any"),
            false => target.to_string(),
        };
        let mode = match self.force {
            true => Mode::KillForce,
            false => Mode::Kill,
        };
        Asked::plain(mode, Label::new(label))
    }

    /// Stopped where its driver runs, when that is here; otherwise on every machine.
    fn batch(&self, batch: &BatchId) -> Result<i32, CallError> {
        let at = self.machine.target()?;
        if let Some(dir) = self.machine.paths.batches().map(|d| d.join(batch.as_str()))
            && let Some(driver) = Driver::of(&dir)
        {
            return driver.stop(self, batch);
        }
        let machines: Vec<MachineName> = match &self.machine.call.on {
            Some(on) => vec![on.clone()],
            None if self.machine.fleet.exists() => self.machine.fleet.names(),
            None => Vec::new(),
        };
        if machines.is_empty() {
            self.machine.somewhere(&at)?;
            return self.machine.send(self.asked(batch.as_str()), &at);
        }
        let named: Vec<&str> = machines.iter().map(MachineName::as_str).collect();
        eprintln!(
            "dibs: batch {batch} is not driven from this computer, so it is stopped on the machines: {}",
            named.join(" ")
        );
        let bound = Bound::polled(KILL_POLL_SECS, Kept::Everything);
        let answers = self.machine.each(&machines, |machine| {
            let asked = self.asked(batch.as_str());
            self.machine.ask(machine, &Call::default(), asked, bound)
        });
        print!(
            "{}",
            Answers {
                answers: &answers,
                spaced: false,
            }
        );
        let stopped = answers.iter().any(|a| a.answer.exit == Some(0));
        Ok(i32::from(
            match stopped {
                true => Exit::Success,
                false => Exit::Failed,
            }
            .code(),
        ))
    }
}

/// A batch's driver running on this computer, as the record it holds in the batch's directory
/// says: whatever its command line reads, it is the process holding that record's lock.
pub struct Driver<'a> {
    dir: &'a Path,
    pid: i32,
}

/// A driver's hold on its batch's record, for as long as the batch runs.
pub struct DriverClaim {
    _record: File,
}

impl<'a> Driver<'a> {
    const RECORD: &'static str = "driver";

    /// Records this process as the driver of the batch in `dir`.
    pub fn claim(dir: &Path) -> io::Result<DriverClaim> {
        let mut record = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join(Driver::RECORD))?;
        record.lock()?;
        record.set_len(0)?;
        writeln!(record, "{}", std::process::id())?;
        Ok(DriverClaim { _record: record })
    }

    /// The driver of the batch in `dir`, while one runs here.
    fn of(dir: &'a Path) -> Option<Driver<'a>> {
        let record = File::open(dir.join(Driver::RECORD)).ok()?;
        match record.try_lock_shared() {
            Err(TryLockError::WouldBlock) => {}
            Ok(()) | Err(TryLockError::Error(_)) => return None,
        }
        let pid = fs::read_to_string(dir.join(Driver::RECORD))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        Some(Driver { dir, pid })
    }

    fn stop(&self, kill: &Kill, batch: &BatchId) -> Result<i32, CallError> {
        let owner = fs::read_to_string(self.dir.join("owner")).unwrap_or_default();
        let owner = owner.trim_end_matches('\n');
        if !owner.is_empty() && owner != kill.machine.caller.id && !kill.anyone {
            eprintln!(
                "dibs: batch {batch} was started by another session. If it should stop:  dibs --kill {batch} --anyone"
            );
            return Ok(i32::from(Exit::Refused.code()));
        }
        fs::write(self.dir.join("cancel"), "")?;
        eprintln!(
            "dibs: cancelling batch {batch} here: nothing more starts, and its running steps are stopped."
        );
        if !ended(self.pid, DRIVER_GRACE) {
            // SAFETY: signals a process by pid; the worst a stale pid costs is a stray SIGTERM.
            unsafe { libc::kill(self.pid, libc::SIGTERM) };
            eprintln!(
                "dibs: its driver did not stop within a minute, so it was sent SIGTERM; its steps die with it."
            );
        }
        if let Ok(summary) = fs::read_to_string(self.dir.join("summary")) {
            print!("{summary}");
        }
        Ok(0)
    }
}

/// Waits up to `within` for a process this one did not start to end.
#[cfg(target_os = "linux")]
fn ended(pid: i32, within: Duration) -> bool {
    // SAFETY: pidfd_open takes a pid and flags, and returns a descriptor this function closes.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    if fd < 0 {
        return true;
    }
    let mut watched = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = i32::try_from(within.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: polls the one descriptor above, then closes it.
    let ready = unsafe {
        let ready = libc::poll(&mut watched, 1, millis);
        libc::close(fd);
        ready
    };
    ready > 0
}

#[cfg(target_os = "macos")]
fn ended(pid: i32, within: Duration) -> bool {
    // SAFETY: one kqueue, watching one pid for its exit, closed before returning.
    unsafe {
        let queue = libc::kqueue();
        if queue < 0 {
            return false;
        }
        let change = libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let mut event = change;
        let timeout = libc::timespec {
            tv_sec: within.as_secs() as libc::time_t,
            tv_nsec: libc::c_long::from(within.subsec_nanos() as i32),
        };
        let n = libc::kevent(queue, &change, 1, &mut event, 1, &timeout);
        libc::close(queue);
        n != 0
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn ended(_pid: i32, _within: Duration) -> bool {
    false
}
