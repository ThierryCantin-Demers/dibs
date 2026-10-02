use dibs_format::Exit;
use std::fmt;

/// A command line dibs refuses, with exit 2. The message is printed as it is, prefix and all,
/// since the bash client's refusals were not uniform and agents match their words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub message: String,
    /// The help follows on stdout, as it does after an unknown option.
    pub with_help: bool,
}

impl CliError {
    pub fn new(message: impl Into<String>) -> CliError {
        CliError {
            message: message.into(),
            with_help: false,
        }
    }

    pub fn with_help(message: impl Into<String>) -> CliError {
        CliError {
            message: message.into(),
            with_help: true,
        }
    }

    /// `--<flag>` given with nothing after it.
    pub fn needs_value(flag: &str) -> CliError {
        CliError::new(format!("dibs: {flag} needs a value."))
    }

    pub fn exit(&self) -> Exit {
        Exit::Refused
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}
