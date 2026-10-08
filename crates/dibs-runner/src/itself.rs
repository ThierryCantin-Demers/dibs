use std::{os::unix::process::CommandExt as _, path::PathBuf, process::Command};

/// This runner's own binary, as it was started: `dibs-runner`, or a client's `dibs __runner`.
pub struct Itself;

impl Itself {
    /// The word a client's own binary serves the runner under.
    const RUNNER_WORD: &str = "__runner";

    fn exe() -> PathBuf {
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("dibs-runner"))
    }

    fn served_by_a_client() -> bool {
        std::env::args().nth(1).as_deref() == Some(Itself::RUNNER_WORD)
    }

    /// The words that start it again, for a job's shell: through this process on Linux, so a
    /// version removed while the job queued still starts.
    pub fn words() -> Vec<String> {
        let exe = match cfg!(target_os = "linux") {
            true => format!("/proc/{}/exe", std::process::id()),
            false => Itself::exe().display().to_string(),
        };
        let mut words = vec![exe];
        if Itself::served_by_a_client() {
            words.push(Itself::RUNNER_WORD.to_string());
        }
        words
    }

    /// The same, started from here: through `/proc` on Linux, so a binary an update has replaced
    /// still starts as the one running.
    pub fn command() -> Command {
        let mut command = match cfg!(target_os = "linux") {
            true => {
                let mut command = Command::new("/proc/self/exe");
                command.arg0(Itself::exe());
                command
            }
            false => Command::new(Itself::exe()),
        };
        if Itself::served_by_a_client() {
            command.arg(Itself::RUNNER_WORD);
        }
        command
    }
}
