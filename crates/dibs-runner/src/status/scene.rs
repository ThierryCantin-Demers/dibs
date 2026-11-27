use dibs_format::{JobId, Label};
use std::{collections::BTreeMap, path::PathBuf};

/// What only the text shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scene {
    /// The machine's short name, which `dibs --on` reaches it by.
    pub host: String,
    /// The processes holding a lock no record names, written in the order the kernel names
    /// them.
    pub orphans: Vec<Orphan>,
    /// Processes of this account that outlived the jobs that started them, written in the
    /// order the process table lists them.
    pub leftovers: Vec<Leftover>,
    /// The lock is taken, and no process this account can see holds it.
    pub unseen: bool,
    /// The lock directory, for a caller that asked with `-v`.
    pub listing: Option<Listing>,
    /// Each holder's `--with` servers, read by the holder's pid, each holder's in the order
    /// `with.<pid>` lists them.
    pub services: BTreeMap<u32, Vec<Service>>,
}

impl Scene {
    /// The `--with` servers of the holder with this pid, none for one that runs none.
    pub fn services_of(&self, pid: u32) -> &[Service] {
        self.services.get(&pid).map_or(&[], Vec::as_slice)
    }
}

/// A process holding the lock that no record names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    pub pid: u32,
    /// Its pid, age, owner and arguments, as `ps -o pid=,etime=,user=,args=` prints them.
    pub described: String,
}

/// A process whose environment names a job that has ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    pub job: JobId,
    pub label: Label,
    /// Its pid, age, owner and arguments, as an orphan's are.
    pub described: String,
}

/// The lock directory's entries, and how many runs the history holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub dir: PathBuf,
    /// In name order, and written in it.
    pub entries: Vec<String>,
    pub history: PathBuf,
    pub run_count: usize,
}

/// A `--with` server run beside a holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub pid: u32,
    pub command: String,
}
