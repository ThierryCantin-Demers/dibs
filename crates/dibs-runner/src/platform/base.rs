use std::path::Path;

/// One process, as the tree under a job is walked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
}

/// What a PCI slot holds now.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "only Linux reads a slot")
)]
pub enum Slot {
    /// This machine cannot say.
    Unreadable,
    Empty,
    /// `vendor:device`, lowercase.
    Holds(String),
}

/// A run of a file's blocks on the disk, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub physical: u64,
    pub length: u64,
    /// Another file's too, as after a reflink copy.
    pub shared: bool,
}

/// What the runner asks of the operating system.
pub trait Platform {
    /// Still in the process table, a zombie included. Another account's processes count, which
    /// a signal could not tell.
    fn exists(pid: u32) -> bool;

    /// Exists and has not yet ended.
    fn running(pid: u32) -> bool;

    /// When it started, in seconds since the epoch.
    fn started_at(pid: u32) -> Option<u64>;

    fn group_of(pid: u32) -> Option<u32>;

    /// Every process on the machine.
    fn processes() -> Vec<Process>;

    /// What a process's descriptor points at: a path, or something that is not one.
    fn fd_path(pid: u32, fd: u32) -> Option<String>;

    /// The TCP ports something listens on.
    fn listening() -> Vec<u16>;

    /// The processes holding an flock on the file; empty where the system cannot say.
    fn lock_holders(file: &Path) -> Vec<u32>;

    fn slot(pci: &str) -> Slot;

    /// `key=value` pairs for the record a measurement keeps.
    fn machine_state() -> String;

    /// Keeps the machine from sleeping until the process exits.
    fn stay_awake(pid: u32);

    /// Its children; None where they cannot be listed without reading every process.
    fn children(pid: u32) -> Option<Vec<u32>>;

    /// What it and the children it has reaped have run on a CPU, in clock ticks.
    fn cpu_ticks(pid: u32) -> Option<u64>;

    /// Clock ticks per second, the unit `cpu_ticks` counts in.
    fn clock_ticks() -> u64;

    /// Its pid, age, owner and arguments, as `ps -o pid=,etime=,user=,args=` prints them.
    fn describe(pid: u32) -> Option<String>;

    /// Whether files on this directory's filesystem can share blocks, as reflinks do on XFS and
    /// btrfs.
    fn shares_blocks(dir: &Path) -> bool;

    /// Where a file's blocks lie on the disk; None where the system cannot say.
    fn extents(file: &Path) -> Option<Vec<Extent>>;

    /// `to` made a copy of `from` that shares its blocks; false where the system cannot.
    fn reflink(from: &Path, to: &Path) -> bool;

    /// The kind of filesystem a directory is on, as `df -T` names it.
    fn filesystem(dir: &Path) -> Option<String>;

    fn cpu_model() -> Option<String>;

    /// A battery means a laptop, which throttles, shares memory between CPU and GPU, and moves.
    fn on_battery() -> bool;

    /// Returns once this process's caller has gone, for a call whose stdin carries something
    /// else: whoever reads its stdout closing it, or its parent, ssh's or the client's, exiting.
    fn await_caller_gone();

    /// The process and everything under it, parents before children, asked downward where the
    /// system can, so a look at a holder reads its own tree and not the whole process table.
    fn tree(root: u32) -> Vec<u32> {
        let mut tree = vec![root];
        let mut next = 0;
        while let Some(&pid) = tree.get(next) {
            next += 1;
            let Some(children) = Self::children(pid) else {
                tree = vec![root];
                tree.extend(crate::job::tree_below(root, &Self::processes()));
                return tree;
            };
            for child in children {
                if !tree.contains(&child) {
                    tree.push(child);
                }
            }
        }
        tree
    }
}

/// `[[dd-]hh:]mm:ss`, as ps prints a process's elapsed time.
#[cfg(target_os = "linux")]
pub fn elapsed(seconds: u64) -> String {
    let (days, hours) = (seconds / 86400, seconds % 86400 / 3600);
    let clock = format!("{:02}:{:02}", seconds % 3600 / 60, seconds % 60);
    match (days, hours) {
        (0, 0) => clock,
        (0, hours) => format!("{hours:02}:{clock}"),
        (days, hours) => format!("{days}-{hours:02}:{clock}"),
    }
}
