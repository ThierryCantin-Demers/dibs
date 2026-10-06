use crate::paths::FileError;
use std::fmt;

/// Why a record this computer keeps could not be read or written.
#[derive(Debug)]
pub enum RecordsError {
    /// No HOME, so nowhere to keep the record.
    NoHome(Kept),
    File(FileError),
    /// A friction note with nothing in it.
    EmptyNote,
}

/// Which record has nowhere to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    Runs,
    Friction,
}

impl From<FileError> for RecordsError {
    fn from(e: FileError) -> RecordsError {
        RecordsError::File(e)
    }
}

impl fmt::Display for RecordsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordsError::NoHome(Kept::Runs) => f.write_str("no HOME, and nowhere to record runs"),
            RecordsError::NoHome(Kept::Friction) => {
                f.write_str("no HOME, and nowhere to record this")
            }
            RecordsError::File(e) => e.fmt(f),
            RecordsError::EmptyNote => {
                f.write_str("--friction takes one line: what got in the way, in your own words")
            }
        }
    }
}
