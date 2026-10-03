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
    /// A held call, or one with `--wait`, `-v`, `--port` or `--with`, needs what the runner does not
    /// do yet: a release, the status display, ports and servers.
    pub fn of(values: &CallValues, held: bool) -> Half {
        let served = match values.mode {
            Mode::Peek => true,
            Mode::Shared | Mode::Bench => {
                values.wait.is_none()
                    && !values.verbose
                    && values.ports.is_empty()
                    && values.services.is_empty()
            }
            _ => false,
        };
        match served && !held {
            true => Half::Runner,
            false => Half::Payload,
        }
    }
}
