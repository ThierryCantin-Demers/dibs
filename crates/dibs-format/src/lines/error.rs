use std::fmt;

/// Why a line did not read as the record it was taken for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineError {
    FieldCount { record: &'static str, found: usize },
    Value { field: &'static str, value: String },
    Missing { field: &'static str },
}

impl fmt::Display for LineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LineError::FieldCount { record, found } => {
                write!(f, "a {record} line has no shape with {found} fields")
            }
            LineError::Value { field, value } => write!(f, "{field} cannot be {value:?}"),
            LineError::Missing { field } => write!(f, "no {field}"),
        }
    }
}

impl std::error::Error for LineError {}
