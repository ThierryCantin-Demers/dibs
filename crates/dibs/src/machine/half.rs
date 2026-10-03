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
    /// One with `--wait` or `-v` needs the status display, which the runner does not print yet.
    pub fn of(values: &CallValues) -> Half {
        let served = match values.mode {
            Mode::Peek => true,
            Mode::Shared | Mode::Bench => values.wait.is_none() && !values.verbose,
            _ => false,
        };
        match served {
            true => Half::Runner,
            false => Half::Payload,
        }
    }
}
