use std::{os::unix::process::CommandExt as _, path::PathBuf, process::Command};

/// This binary, for the processes it starts. Through `/proc` on Linux, so a build an update has
/// renamed over it still starts the one running, as long as this process lives.
pub struct Itself;

impl Itself {
    fn exe() -> PathBuf {
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("dibs"))
    }

    /// The path a process this one starts runs it by.
    pub fn path() -> PathBuf {
        match cfg!(target_os = "linux") {
            true => PathBuf::from(format!("/proc/{}/exe", std::process::id())),
            false => Itself::exe(),
        }
    }

    pub fn command() -> Command {
        let mut command = Command::new(Itself::path());
        command.arg0(Itself::exe());
        command
    }
}
