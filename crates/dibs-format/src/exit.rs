#[cfg(unix)]
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

/// Every exit dibs gives of its own. A command's own status passes through unchanged, so a code
/// here can also be a command's, and the trailer's `by=` is what tells them apart.
///
/// # Guarantees
///
/// - No two exits share a code, so `Exit::from_code(exit.as_code().into())` is `exit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exit {
    Success,
    /// A batch step failed, or a check found something wrong.
    Failed,
    /// Refused or malformed: nothing was sent.
    Refused,
    /// Setup failed: no clone, a pin that did not take, no files kept, no worktree named.
    Setup,
    /// The machine is off, asleep, or its network needs a login.
    Unreachable,
    /// The machine's scratch is full or over quota, so nothing can run there.
    NoRoom,
    /// The lock directory cannot be written, so nothing ran.
    NoLock,
    /// dibs could not install its runner on the machine, so nothing ran.
    NoRunner,
    /// The machine stayed busy past `--wait`.
    Busy,
    /// Its batch was cancelled with `dibs --kill <batch-id>`.
    Cancelled,
    /// A `--with` service exited early or never became ready.
    ServiceFailed,
    /// A recipe's measurement was refused: another tree built into its target since.
    TargetRebuilt,
    /// The machine is closed by its owners, so the call was refused or its job stopped.
    Closed,
    /// The job had run as long as `--check-in` gives it and still runs, now without its call.
    CheckedIn,
    /// The command overran `--max` and was killed while holding the lock.
    Overran,
    /// Stopped by SIGHUP.
    HungUp,
    /// Stopped by SIGINT.
    Interrupted,
    /// Stopped by SIGTERM.
    Terminated,
}

impl Exit {
    /// Every variant, in the order of their codes.
    pub const ALL: [Exit; 18] = [
        Exit::Success,
        Exit::Failed,
        Exit::Refused,
        Exit::Setup,
        Exit::Unreachable,
        Exit::NoRoom,
        Exit::NoLock,
        Exit::NoRunner,
        Exit::Busy,
        Exit::Cancelled,
        Exit::ServiceFailed,
        Exit::TargetRebuilt,
        Exit::Closed,
        Exit::CheckedIn,
        Exit::Overran,
        Exit::HungUp,
        Exit::Interrupted,
        Exit::Terminated,
    ];

    pub fn as_code(self) -> u8 {
        match self {
            Exit::Success => 0,
            Exit::Failed => 1,
            Exit::Refused => 2,
            Exit::Setup => 3,
            Exit::Unreachable => 69,
            Exit::NoRoom => 70,
            Exit::NoLock => 71,
            Exit::NoRunner => 72,
            Exit::Busy => 75,
            Exit::Cancelled => 76,
            Exit::ServiceFailed => 77,
            Exit::TargetRebuilt => 78,
            Exit::Closed => 79,
            Exit::CheckedIn => 80,
            Exit::Overran => 124,
            Exit::HungUp => 129,
            Exit::Interrupted => 130,
            Exit::Terminated => 143,
        }
    }

    /// The code as a process status.
    pub fn as_status(self) -> i32 {
        i32::from(self.as_code())
    }

    /// The code a shell reports for a process that ended: its own, or 128 and its signal.
    pub fn to_shell_code(status: ExitStatus) -> i32 {
        #[cfg(unix)]
        let signal = status.signal();
        // Only a Unix process ends by a signal; nothing else runs one to read.
        #[cfg(not(unix))]
        let signal: Option<i32> = None;
        status
            .code()
            .unwrap_or_else(|| 128 + signal.unwrap_or_default())
    }

    /// The exit dibs gives with this code, if it gives one.
    pub fn from_code(code: i32) -> Option<Exit> {
        Exit::ALL.into_iter().find(|e| e.as_status() == code)
    }

    /// What it means, in the words `--help` uses.
    pub fn meaning(self) -> &'static str {
        match self {
            Exit::Success => "success",
            Exit::Failed => "a step failed",
            Exit::Refused => "refused or malformed",
            Exit::Setup => "setup failed",
            Exit::Unreachable => "unreachable",
            Exit::NoRoom => "no room on the target",
            Exit::NoLock => "no lock, nothing ran",
            Exit::NoRunner => "no runner there, nothing ran",
            Exit::Busy => "busy",
            Exit::Cancelled => "its batch was cancelled",
            Exit::ServiceFailed => "a --with service failed",
            Exit::TargetRebuilt => {
                "a recipe's measurement refused: another tree built into its target since"
            }
            Exit::Closed => "the machine is closed by its owners",
            Exit::CheckedIn => "checked in: the job still runs, and --follow waits for it",
            Exit::Overran => "overran --max",
            Exit::HungUp => "hung up",
            Exit::Interrupted => "interrupted",
            Exit::Terminated => "terminated",
        }
    }
}

impl From<Exit> for std::process::ExitCode {
    fn from(exit: Exit) -> Self {
        std::process::ExitCode::from(exit.as_code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_exit_is_found_by_its_code_and_no_two_share_one() {
        for exit in Exit::ALL {
            assert_eq!(Exit::from_code(exit.as_status()), Some(exit));
        }
    }

    #[test]
    fn a_code_dibs_never_gives_is_no_exit() {
        assert_eq!(Exit::from_code(255), None);
    }

    /// Stops compiling when a variant is added, until it is counted here and listed in `ALL`.
    #[test]
    fn every_variant_is_in_all() {
        let counted = |exit: Exit| match exit {
            Exit::Success
            | Exit::Failed
            | Exit::Refused
            | Exit::Setup
            | Exit::Unreachable
            | Exit::NoRoom
            | Exit::NoLock
            | Exit::NoRunner
            | Exit::Busy
            | Exit::Cancelled
            | Exit::ServiceFailed
            | Exit::TargetRebuilt
            | Exit::Closed
            | Exit::CheckedIn
            | Exit::Overran
            | Exit::HungUp
            | Exit::Interrupted
            | Exit::Terminated => 1,
        };
        assert_eq!(Exit::ALL.into_iter().map(counted).sum::<usize>(), 18);
    }

    #[cfg(unix)]
    #[test]
    fn a_process_ended_by_a_signal_has_128_and_its_signal() {
        let status = ExitStatus::from_raw(15);
        assert_eq!(Exit::to_shell_code(status), 143);
    }
}
