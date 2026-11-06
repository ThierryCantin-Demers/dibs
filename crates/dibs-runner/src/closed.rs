//! A machine its owners have taken back. `closed` in the lock directory keeps it closed until it
//! is removed: no call from an account it does not except starts, and with `now` the jobs
//! running stop. An empty file, as `touch` leaves, closes it to everyone.

use crate::{
    lock::{LockDir, RecordFile},
    machine::{account_name, own_account},
    sink::Sink,
    stop::Stopper,
};
use dibs_format::{Exit, LockRecord, Moment, Span, status::Closure, wire::Close};
use std::{
    fs, io,
    os::unix::fs::MetadataExt as _,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, UNIX_EPOCH},
};

/// How often a waiting call, and a running job, look whether the machine is still closed.
pub const POLL: Duration = Duration::from_secs(1);

/// The lock directory's `closed`.
pub struct Shutter<'a> {
    pub dir: &'a LockDir,
}

impl Shutter<'_> {
    fn path(&self) -> PathBuf {
        self.dir.path.join("closed")
    }

    /// The closure, its account and time read from the file itself, which no line in it can
    /// claim.
    pub fn read(&self) -> Option<Closure> {
        let path = self.path();
        let meta = fs::metadata(&path).ok()?;
        Some(Closure {
            by: account_name(meta.uid()),
            since: meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs()),
            close: Close::of_text(&fs::read_to_string(&path).unwrap_or_default()),
        })
    }

    /// Closed to this account: its calls are refused or wait.
    pub fn shut(&self) -> Option<Closure> {
        let account = own_account();
        self.read().filter(|c| !admits(c, &account))
    }

    pub fn close(&self, close: &Close) -> io::Result<()> {
        let text = close.text();
        let path = self.path();
        let fresh = path.with_extension(format!("{}.new", std::process::id()));
        fs::write(&fresh, text)?;
        // Replaced whole, so another account's closure, which this one cannot rewrite, goes.
        fs::rename(&fresh, &path).inspect_err(|_| {
            let _ = fs::remove_file(&fresh);
        })
    }

    /// Whether it was closed.
    pub fn open(&self) -> io::Result<bool> {
        match fs::remove_file(self.path()) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The jobs holding the lock whose accounts the closure does not except, with each account.
    pub fn running(&self, closure: &Closure) -> Vec<(LockRecord, String)> {
        self.dir
            .named("holder")
            .into_iter()
            .filter(|file| RecordFile(file).alive())
            .filter_map(|file| {
                let account = account_name(fs::metadata(&file).ok()?.uid());
                let record = fs::read_to_string(&file)
                    .ok()?
                    .lines()
                    .next()?
                    .parse()
                    .ok()?;
                Some((record, account))
            })
            .filter(|(_, account)| !admits(closure, account))
            .collect()
    }
}

/// Its accounts are excepted from the closure.
pub fn admits(closure: &Closure, account: &str) -> bool {
    closure.close.except.iter().any(|a| a == account)
}

/// Why a call was refused, or is waiting.
pub fn said(closure: &Closure) -> String {
    let mut said = format!(
        "dibs: this machine was closed by {}, {} ago, and nothing new starts until it is opened.\n",
        closure.by,
        Span(Moment::epoch_now().saturating_sub(closure.since))
    );
    if let Some(reason) = &closure.close.reason {
        said.push_str(&format!("  {reason}\n"));
    }
    said
}

/// `--close` and `--open`, answered beside whatever holds the machine.
pub struct Owners<'a> {
    pub dir: &'a LockDir,
    pub sink: &'a Sink,
}

impl Owners<'_> {
    /// Closes the machine, then waits for the jobs it does not except to end, which `now` stops.
    pub fn close(&self, close: &Close) -> i32 {
        let shutter = Shutter { dir: self.dir };
        if let Err(e) = shutter.close(close) {
            self.sink.say(&format!(
                "dibs: could not close {}: {e}\n",
                self.dir.path.display()
            ));
            return Exit::NoLock.status();
        }
        let Some(closure) = shutter.read() else {
            return Exit::NoLock.status();
        };
        let running = shutter.running(&closure);
        if running.is_empty() {
            self.sink.say(
                "dibs: closed, and nothing of dibs's runs here now. dibs --open opens it again.\n",
            );
            return Exit::Success.status();
        }
        let ending = match close.now {
            true => "being stopped",
            false => "still to finish",
        };
        let mut said = format!("dibs: closed. {} job(s) {ending}:\n", running.len());
        for (record, account) in &running {
            said.push_str(&format!(
                "  pid {}  {}  {account} ({})\n",
                record.pid, record.label, record.agent
            ));
        }
        said.push_str("  Waiting for them. Ctrl-C stops the wait; the machine stays closed.\n");
        self.sink.say(&said);
        loop {
            if shutter.read().is_none() {
                self.sink.say("dibs: opened again meanwhile.\n");
                return Exit::Success.status();
            }
            if shutter.running(&closure).is_empty() {
                self.sink.say("dibs: nothing of dibs's runs here now.\n");
                return Exit::Success.status();
            }
            thread::sleep(POLL);
        }
    }

    /// Opens the machine, which another account's closure needs `anyone` for.
    pub fn open(&self, anyone: bool) -> i32 {
        let shutter = Shutter { dir: self.dir };
        let Some(closure) = shutter.read() else {
            self.sink.say("dibs: this machine was not closed.\n");
            return Exit::Success.status();
        };
        if closure.by != own_account() && !anyone {
            self.sink.say(&format!(
                "dibs: {} closed this machine, not this account, so it stays closed.\n  \
                 Opening it is theirs to decide. If they asked you to:  dibs --open --anyone\n",
                closure.by
            ));
            return Exit::Refused.status();
        }
        match shutter.open() {
            Ok(_) => {
                self.sink
                    .say("dibs: open. Calls waiting for it start in their turn.\n");
                Exit::Success.status()
            }
            Err(e) => {
                self.sink.say(&format!(
                    "dibs: could not open {}: {e}\n",
                    self.dir.path.display()
                ));
                Exit::NoLock.status()
            }
        }
    }
}

/// Stops the running job once its machine is closed with `now`, which nothing else tells it.
pub struct Watch {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Watch {
    pub fn start(dir: LockDir, stopper: Arc<Stopper>) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if let Some(closure) = (Shutter { dir: &dir }).shut().filter(|c| c.close.now) {
                    stopper.closed(&format!("closed by {}", closure.by));
                    return;
                }
                thread::park_timeout(POLL);
            }
        });
        Watch { stop, thread }
    }

    /// Woken rather than waited out, since the lock is held until it returns.
    pub fn end(self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.thread().unpark();
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_closes_to_everyone_and_a_written_one_says_whom_it_spares() {
        let path = std::env::temp_dir().join(format!("dibs-closed-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let dir = LockDir { path: path.clone() };
        let shutter = Shutter { dir: &dir };
        assert!(shutter.read().is_none());
        fs::write(path.join("closed"), "").unwrap();
        let touched = shutter.read().unwrap();
        assert_eq!(touched.by, own_account());
        assert!(!touched.close.now && touched.close.except.is_empty());
        assert!(shutter.shut().is_some());
        shutter
            .close(&Close {
                now: true,
                except: vec![own_account(), "other".into()],
                reason: Some("their tests".into()),
            })
            .unwrap();
        let written = shutter.read().unwrap();
        assert!(written.close.now);
        assert_eq!(written.close.reason.as_deref(), Some("their tests"));
        assert!(shutter.shut().is_none(), "this account is excepted");
        assert!(shutter.open().unwrap());
        assert!(!shutter.open().unwrap());
        let _ = fs::remove_dir_all(&path);
    }
}
