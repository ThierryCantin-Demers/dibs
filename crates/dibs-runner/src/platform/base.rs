use std::path::Path;

/// One process, as the tree under a job is walked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
}

/// What a PCI slot holds now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// This machine cannot say.
    Unreadable,
    Empty,
    /// `vendor:device`, lowercase.
    Holds(String),
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
}
