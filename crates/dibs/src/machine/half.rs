use crate::machine::payload::CallValues;
use dibs_format::Mode;

/// Which half of dibs serves a call on the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    /// `dibs-runner`, sent the call as a request.
    Runner,
    /// `lib/machine`, sent as a bash payload ahead of the call's values.
    Payload,
}

impl Half {
    /// The runner serves the calls it implements; every other call still ships `lib/machine`.
    pub fn of(values: &CallValues) -> Half {
        let served = matches!(
            values.mode,
            Mode::Peek
                | Mode::Shared
                | Mode::Bench
                | Mode::Rsh
                | Mode::Out
                | Mode::Fetch
                | Mode::Status
                | Mode::Watch
                | Mode::Log
                | Mode::Kill
                | Mode::KillForce
                | Mode::Release
        );
        match served {
            true => Half::Runner,
            false => Half::Payload,
        }
    }
}
