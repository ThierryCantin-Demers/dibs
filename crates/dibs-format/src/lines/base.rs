use std::{fmt, str::FromStr};

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

impl LineError {
    /// A field's value read as what it holds.
    pub fn parse<T: FromStr>(field: &'static str, value: &str) -> Result<T, LineError> {
        value.parse().map_err(|_| LineError::Value {
            field,
            value: value.to_string(),
        })
    }
}

/// The fields of one tab-separated line, read in order.
pub struct Fields<'a> {
    fields: Vec<&'a str>,
    next: usize,
}

impl<'a> Fields<'a> {
    pub fn of(line: &'a str) -> Fields<'a> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        Fields {
            fields: line.split('\t').collect(),
            next: 0,
        }
    }

    pub fn count(&self) -> usize {
        self.fields.len()
    }

    pub fn text(&mut self) -> &'a str {
        let field = self.fields.get(self.next).copied().unwrap_or_default();
        self.next += 1;
        field
    }

    pub fn parsed<T: FromStr>(&mut self, field: &'static str) -> Result<T, LineError> {
        LineError::parse(field, self.text())
    }

    /// A field that holds `-` when there is nothing to say.
    pub fn dashed<T: FromStr>(&mut self, field: &'static str) -> Result<Option<T>, LineError> {
        match self.text() {
            "-" => Ok(None),
            value => LineError::parse(field, value).map(Some),
        }
    }

    /// A field that is empty when there is nothing to say.
    pub fn optional(&mut self) -> Option<String> {
        Some(self.text())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    }
}

/// Free text made safe for one field: a tab or a newline in it would end the field or the line.
pub struct Field<'a>(pub &'a str);

impl fmt::Display for Field<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            match c {
                '\t' | '\n' => f.write_str(" ")?,
                c => fmt::Write::write_char(f, c)?,
            }
        }
        Ok(())
    }
}

/// A value written as `-` when absent.
pub struct Dashed<'a, T>(pub Option<&'a T>);

impl<T: fmt::Display> fmt::Display for Dashed<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{}", Field(&v.to_string())),
            None => f.write_str("-"),
        }
    }
}
