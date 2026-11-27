use crate::{
    JobId,
    lines::{error::LineError, field::DASH},
};
use std::{ops::RangeBounds, str::FromStr};

/// Reads the fields of one tab-separated line in order.
pub struct FieldReader<'a> {
    /// The line's fields, read by position.
    fields: Vec<&'a str>,
    next: usize,
}

impl<'a> FieldReader<'a> {
    pub fn of_line(line: &'a str) -> FieldReader<'a> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        FieldReader {
            fields: line.split('\t').collect(),
            next: 0,
        }
    }

    /// The line as a `record` line, which has one of the field counts in `fields`.
    pub fn of_record(
        line: &'a str,
        record: &'static str,
        fields: impl RangeBounds<usize>,
    ) -> Result<FieldReader<'a>, LineError> {
        let reader = FieldReader::of_line(line);
        let found = reader.count();
        if !fields.contains(&found) {
            return Err(LineError::FieldCount { record, found });
        }
        Ok(reader)
    }

    pub fn count(&self) -> usize {
        self.fields.len()
    }

    /// The next field, empty past the last.
    pub fn read_text(&mut self) -> &'a str {
        let field = self.fields.get(self.next).copied().unwrap_or_default();
        self.next += 1;
        field
    }

    pub fn read_parsed<T: FromStr>(&mut self, field: &'static str) -> Result<T, LineError> {
        parse_field(field, self.read_text())
    }

    /// A field that holds `-` when there is nothing to say.
    pub fn read_dashed<T: FromStr>(&mut self, field: &'static str) -> Result<Option<T>, LineError> {
        match self.read_text() {
            DASH => Ok(None),
            value => parse_field(field, value).map(Some),
        }
    }

    /// A field that is empty when there is nothing to say.
    pub fn read_optional(&mut self) -> Option<String> {
        non_empty(self.read_text())
    }

    /// Free text written as `-` when empty, read back from either.
    pub fn read_filled(&mut self) -> Option<String> {
        Some(self.read_text())
            .filter(|text| *text != DASH)
            .and_then(non_empty)
    }

    /// A job's id written as `-` when absent, read as none when it is no job's id.
    pub fn read_job_id(&mut self) -> Option<JobId> {
        self.read_filled()
            .and_then(|job| JobId::checked_from_text(&job))
    }

    /// A dashed field that only a line of at least `field_count` fields holds: none in a shorter
    /// line, which leaves its next field to the field after.
    pub fn read_dashed_if_fields_at_least<T: FromStr>(
        &mut self,
        field: &'static str,
        field_count: usize,
    ) -> Result<Option<T>, LineError> {
        match self.count() >= field_count {
            true => self.read_dashed(field),
            false => Ok(None),
        }
    }

    /// A filled field that only a line of at least `field_count` fields holds: none in a shorter
    /// line, which leaves its next field to the field after.
    pub fn read_filled_if_fields_at_least(&mut self, field_count: usize) -> Option<String> {
        match self.count() >= field_count {
            true => self.read_filled(),
            false => None,
        }
    }
}

/// A field's value read as what it holds.
pub fn parse_field<T: FromStr>(field: &'static str, value: &str) -> Result<T, LineError> {
    value.parse().map_err(|_| LineError::Value {
        field,
        value: value.to_string(),
    })
}

/// The text, or None when it is empty.
pub fn non_empty(text: &str) -> Option<String> {
    Some(text)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// `line` with the field at `index` replaced by `value`, for a test that corrupts one field.
#[cfg(test)]
pub fn line_with_field(line: &str, index: usize, value: &str) -> String {
    let mut fields: Vec<&str> = line.split('\t').collect();
    fields[index] = value;
    fields.join("\t")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_of_a_count_its_record_has_reads() {
        assert!(FieldReader::of_record("a\tb", "pair", 2..=2).is_ok());
    }

    #[test]
    fn a_line_of_a_count_its_record_has_not_is_refused() {
        assert_eq!(
            FieldReader::of_record("a\tb\tc", "pair", 2..=2).err(),
            Some(LineError::FieldCount {
                record: "pair",
                found: 3
            })
        );
    }

    #[test]
    fn a_field_past_the_last_reads_empty() {
        let mut f = FieldReader::of_line("a");
        f.read_text();
        assert_eq!(f.read_text(), "");
    }

    #[test]
    fn a_dash_reads_as_no_value() {
        assert_eq!(FieldReader::of_line("-").read_dashed::<u64>("n"), Ok(None));
    }

    #[test]
    fn a_dashed_value_that_does_not_read_is_refused() {
        assert_eq!(
            FieldReader::of_line("x").read_dashed::<u64>("n"),
            Err(LineError::Value {
                field: "n",
                value: "x".into()
            })
        );
    }

    #[test]
    fn a_dash_reads_as_no_filled_text() {
        assert_eq!(FieldReader::of_line("-").read_filled(), None);
    }

    #[test]
    fn an_empty_field_reads_as_no_filled_text() {
        assert_eq!(FieldReader::of_line("").read_filled(), None);
    }

    #[test]
    fn a_field_that_is_no_jobs_id_reads_as_no_job() {
        assert_eq!(FieldReader::of_line("../42").read_job_id(), None);
    }

    #[test]
    fn a_jobs_id_reads_as_its_job() {
        assert_eq!(
            FieldReader::of_line("20261001120000-42").read_job_id(),
            Some(JobId::new("20261001120000-42"))
        );
    }

    #[test]
    fn a_dashed_field_a_shorter_line_lacks_reads_as_none_and_is_left_unread() {
        let mut f = FieldReader::of_line("cmd");
        assert_eq!(f.read_dashed_if_fields_at_least::<u64>("n", 2), Ok(None));
        assert_eq!(f.read_text(), "cmd");
    }

    #[test]
    fn a_filled_field_a_shorter_line_lacks_reads_as_none_and_is_left_unread() {
        let mut f = FieldReader::of_line("cmd");
        assert_eq!(f.read_filled_if_fields_at_least(2), None);
        assert_eq!(f.read_text(), "cmd");
    }
}
