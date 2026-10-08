use crate::platform::{Host, Platform as _};
use dibs_format::{LockRecord, Span};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// A record outlives a pid that comes round again only by being older than the process that has
/// it now; two seconds of slack for the rounding.
const SAME_PROCESS_SLACK: u64 = 2;
/// A batch's cancellation refuses its later steps here for a day.
const CANCELLED_FOR: Duration = Duration::from_secs(Span::DAY.0);

/// The lock directory: the lock files, and the records that say who holds and who waits.
#[derive(Debug, Clone)]
pub struct LockDir {
    pub path: PathBuf,
}

/// Who holds `rw` this moment, as the kernel says.
#[derive(Debug, Clone, Default)]
pub struct Takers {
    /// Some process can be seen holding it.
    pub seen: bool,
    /// Those that no record accounts for, outside the asking process's own group.
    pub orphans: Vec<u32>,
}

/// The two records a job names itself by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Waiting,
    Holder,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Waiting => "waiting",
            Kind::Holder => "holder",
        }
    }
}

impl LockDir {
    pub fn gate(&self) -> PathBuf {
        self.path.join("gate")
    }

    pub fn rw(&self) -> PathBuf {
        self.path.join("rw")
    }

    pub fn file(&self, kind: &str, pid: u32) -> PathBuf {
        self.path.join(format!("{kind}.{pid}"))
    }

    pub fn write(&self, kind: Kind, record: &LockRecord) {
        let _ = fs::write(self.file(kind.prefix(), record.pid), format!("{record}\n"));
    }

    /// Waiting becomes holding, restamped: a holder's clock starts when it takes the lock.
    pub fn hold(&self, record: &LockRecord) {
        self.write(Kind::Waiting, record);
        let _ = fs::rename(
            self.file(Kind::Waiting.prefix(), record.pid),
            self.file(Kind::Holder.prefix(), record.pid),
        );
    }

    /// Every record of a kind, in the order the shell's glob lists them.
    pub fn records(&self, kind: Kind) -> Vec<LockRecord> {
        self.named(kind.prefix())
            .into_iter()
            .filter_map(|file| fs::read_to_string(&file).ok()?.lines().next()?.parse().ok())
            .collect()
    }

    /// The files whose names start with `<prefix>.`, sorted by name.
    pub fn named(&self, prefix: &str) -> Vec<PathBuf> {
        let lead = format!("{prefix}.");
        let mut files: Vec<PathBuf> = fs::read_dir(&self.path)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&lead))
            .map(|e| e.path())
            .collect();
        files.sort();
        files
    }

    /// The batch's later steps are refused here.
    pub fn cancelled(&self, batch: &str) -> bool {
        self.path.join(format!("cancelled.{batch}")).exists()
    }

    /// Everything this process wrote, gone, as its end leaves the directory.
    pub fn clear(&self, pid: u32) {
        for kind in ["waiting", "holder", "work", "cpu", "batch", "hold", "with"] {
            let _ = fs::remove_file(self.file(kind, pid));
        }
        for port in self.named("port") {
            if fs::read_to_string(&port).is_ok_and(|by| by.trim() == pid.to_string()) {
                let _ = fs::remove_file(&port);
            }
        }
    }

    /// Who holds `rw`. A process taking it and letting go at once is a client testing it, and the
    /// asking process's own group is the invocation asking, so neither is an orphan.
    pub fn takers(&self, asking: u32) -> Takers {
        let mine = Host::group_of(asking);
        let holding: Vec<u32> = Host::lock_holders(&self.rw())
            .into_iter()
            .filter(|&pid| Host::exists(pid))
            .collect();
        let orphans = holding
            .iter()
            .copied()
            .filter(|&pid| mine.is_none() || Host::group_of(pid) != mine)
            .filter(|&pid| {
                !self.file("holder", pid).exists() && !self.file("waiting", pid).exists()
            })
            .collect();
        Takers {
            seen: !holding.is_empty(),
            orphans,
        }
    }

    /// Drops what dead jobs left: a record outlives its process only when that was killed outright.
    pub fn prune(&self) {
        for file in self.named("cancelled") {
            if RecordFile(&file).age().is_some_and(|a| a > CANCELLED_FOR) {
                let _ = fs::remove_file(&file);
            }
        }
        for file in self.named("port") {
            let alive = fs::read_to_string(&file)
                .ok()
                .and_then(|pid| pid.trim().parse().ok())
                .is_some_and(Host::exists);
            if !alive {
                let _ = fs::remove_file(&file);
            }
        }
        for file in [self.named("holder"), self.named("waiting")].concat() {
            if !RecordFile(&file).alive() {
                let _ = fs::remove_file(&file);
            }
        }
        for prefix in ["cpu", "batch", "hold", "with"] {
            for file in self.named(prefix) {
                if !RecordFile(&file).pid().is_some_and(Host::exists) {
                    let _ = fs::remove_file(&file);
                }
            }
        }
    }
}

/// A record's file in the lock directory, named `<kind>.<pid>`.
#[derive(Debug, Clone, Copy)]
pub struct RecordFile<'a>(pub &'a Path);

impl RecordFile<'_> {
    /// The pid its name ends in.
    pub fn pid(self) -> Option<u32> {
        self.0.extension()?.to_str()?.parse().ok()
    }

    fn age(self) -> Option<Duration> {
        fs::metadata(self.0)
            .ok()?
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
    }

    /// Whether the pid still names the process that wrote it: a pid comes round in days on a
    /// busy machine, and a record left by a job killed outright would name whoever has it by
    /// then.
    pub fn written_by(self, pid: u32) -> bool {
        let Some(began) = Host::started_at(pid) else {
            return false;
        };
        let Some(wrote) = fs::metadata(self.0)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        else {
            return true;
        };
        began <= wrote.as_secs() + SAME_PROCESS_SLACK
    }

    /// Whether the process that wrote it is still there.
    fn alive(self) -> bool {
        self.pid().is_some_and(|pid| self.written_by(pid))
    }
}
