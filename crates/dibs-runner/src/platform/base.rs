use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

/// How long a reap waits between its rounds.
const REAP_ROUND: Duration = Duration::from_millis(250);
/// Rounds of TERM to what is below, then of KILL, before the named processes themselves.
const REAP_BELOW_ROUNDS: usize = 50;
const REAP_BELOW_TERM_ROUNDS: usize = 40;
/// Rounds the named processes are given to end after TERM before KILL.
const REAP_NAMED_ROUNDS: usize = 41;

/// One process, as the tree under a job is walked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
}

/// The processes under a pid in a process table.
pub trait Descendants {
    /// Parents before children, the pid itself left out.
    fn below(&self, root: u32) -> Vec<u32>;
}

impl Descendants for [Process] {
    fn below(&self, root: u32) -> Vec<u32> {
        let mut wanted = BTreeSet::from([root]);
        let mut below = Vec::new();
        let mut level = vec![root];
        while !level.is_empty() {
            let next: Vec<u32> = self
                .iter()
                .filter(|p| level.contains(&p.parent) && !wanted.contains(&p.pid))
                .map(|p| p.pid)
                .collect();
            wanted.extend(next.iter().copied());
            below.extend(next.iter().copied());
            level = next;
        }
        below
    }
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

    /// The directory a process works in; None where the system will not say.
    fn cwd(pid: u32) -> Option<PathBuf>;

    /// A variable of a process's environment; None where the system will not say.
    fn variable(pid: u32, name: &str) -> Option<String>;

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
                tree.extend(Self::processes().below(root));
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

    /// Stops a job's whole tree. The lock goes the moment the job exits, and a grandchild still
    /// running then would run unlocked beside the next measurement, so everything below goes
    /// first, deepest first, then what was named: TERM, and KILL for whatever outlives it.
    fn reap(pids: &[u32]) {
        for round in 1..=REAP_BELOW_ROUNDS {
            let processes = Self::processes();
            let below: Vec<u32> = pids.iter().flat_map(|p| processes.below(*p)).collect();
            if below.is_empty() {
                break;
            }
            let sig = match round > REAP_BELOW_TERM_ROUNDS {
                true => libc::SIGKILL,
                false => libc::SIGTERM,
            };
            for pid in below.iter().rev() {
                Self::signal(*pid, sig);
            }
            thread::sleep(REAP_ROUND);
        }
        for pid in pids {
            Self::signal(*pid, libc::SIGTERM);
        }
        for round in 1..=REAP_NAMED_ROUNDS {
            let alive: Vec<u32> = pids.iter().copied().filter(|p| Self::running(*p)).collect();
            if alive.is_empty() {
                return;
            }
            if round == REAP_NAMED_ROUNDS {
                for pid in alive {
                    Self::signal(pid, libc::SIGKILL);
                }
            }
            thread::sleep(REAP_ROUND);
        }
    }

    fn signal(pid: u32, signal: libc::c_int) {
        // SAFETY: kill only sends a signal.
        unsafe { libc::kill(pid as libc::pid_t, signal) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tree_below_lists_parents_before_children() {
        let p = |pid, parent| Process { pid, parent };
        let table = [p(1, 0), p(10, 1), p(11, 10), p(12, 11), p(13, 10), p(20, 1)];
        assert_eq!(table.below(10), vec![11, 13, 12]);
        assert!(table.below(12).is_empty());
    }
}
