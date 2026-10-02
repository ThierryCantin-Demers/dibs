/// Every exit dibs gives of its own. A command's own status passes through unchanged, so a code
/// here can also be a command's, and the trailer's `by=` is what tells them apart.
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
    /// The machine stayed busy past `--wait`.
    Busy,
    /// Its batch was cancelled with `dibs --kill <batch-id>`.
    Cancelled,
    /// A `--with` service exited early or never became ready.
    ServiceFailed,
    /// A recipe's measurement was refused: another tree built into its target since.
    TargetRebuilt,
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
    pub const ALL: [Exit; 15] = [
        Exit::Success,
        Exit::Failed,
        Exit::Refused,
        Exit::Setup,
        Exit::Unreachable,
        Exit::NoRoom,
        Exit::NoLock,
        Exit::Busy,
        Exit::Cancelled,
        Exit::ServiceFailed,
        Exit::TargetRebuilt,
        Exit::Overran,
        Exit::HungUp,
        Exit::Interrupted,
        Exit::Terminated,
    ];

    pub fn code(self) -> u8 {
        match self {
            Exit::Success => 0,
            Exit::Failed => 1,
            Exit::Refused => 2,
            Exit::Setup => 3,
            Exit::Unreachable => 69,
            Exit::NoRoom => 70,
            Exit::NoLock => 71,
            Exit::Busy => 75,
            Exit::Cancelled => 76,
            Exit::ServiceFailed => 77,
            Exit::TargetRebuilt => 78,
            Exit::Overran => 124,
            Exit::HungUp => 129,
            Exit::Interrupted => 130,
            Exit::Terminated => 143,
        }
    }

    /// The exit dibs gives with this code, if it gives one.
    pub fn of_code(code: i32) -> Option<Exit> {
        Exit::ALL.into_iter().find(|e| i32::from(e.code()) == code)
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
            Exit::Busy => "busy",
            Exit::Cancelled => "its batch was cancelled",
            Exit::ServiceFailed => "a --with service failed",
            Exit::TargetRebuilt => {
                "a recipe's measurement refused: another tree built into its target since"
            }
            Exit::Overran => "overran --max",
            Exit::HungUp => "hung up",
            Exit::Interrupted => "interrupted",
            Exit::Terminated => "terminated",
        }
    }
}

impl From<Exit> for std::process::ExitCode {
    fn from(exit: Exit) -> Self {
        std::process::ExitCode::from(exit.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_exit_is_found_by_its_code_and_no_two_share_one() {
        for exit in Exit::ALL {
            assert_eq!(Exit::of_code(exit.code().into()), Some(exit));
        }
        assert_eq!(Exit::of_code(255), None);
    }
}
