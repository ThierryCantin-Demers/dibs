use crate::{
    Label, Mode,
    lines::{error::LineError, field::Field, reader::FieldReader},
};
use std::str::FromStr;

/// One successful run in a machine's duration history, which every estimate is drawn from.
///
/// # Guarantees
///
/// - Writes the 5 fields a machine writes today, byte for byte, and reads those and the 3 or 4
///   of a line with no agent or fingerprint.
/// - Free text never adds a field or a line: a tab, a newline or a carriage return in it is
///   written as a space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryLine {
    pub mode: Mode,
    pub label: Label,
    pub secs: u64,
    pub agent: Option<String>,
    pub fingerprint: Option<String>,
}

impl HistoryLine {
    /// The line the history file holds for this run, without its newline.
    pub fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.mode,
            Field::from_text(self.label.as_str()),
            self.secs,
            Field::from_optional(self.agent.as_deref()),
            Field::from_optional(self.fingerprint.as_deref()),
        )
    }
}

impl FromStr for HistoryLine {
    type Err = LineError;

    /// # Errors
    ///
    /// [`LineError::FieldCount`] for fewer than 3 fields or more than 5, and [`LineError::Value`]
    /// for a mode or a duration that does not read.
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut f = FieldReader::of_record(line, "history", 3..=5)?;
        Ok(HistoryLine {
            mode: f.read_parsed("mode")?,
            label: Label::new(f.read_text()),
            secs: f.read_parsed("seconds")?,
            agent: f.read_optional(),
            fingerprint: f.read_optional(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::reader::line_with_field;

    const LINE: &str = "shared\tapp_build_plain\t12\tsession suite\te8e800432f19a324";

    #[test]
    fn the_lines_a_machine_writes_today_read_and_write_back_byte_for_byte() {
        for line in [
            LINE,
            "shared\tquickie\t3\tsession suite\t",
            "bench\trec-queued-bench\t40\tsession suite\t",
        ] {
            assert_eq!(line.parse::<HistoryLine>().unwrap().to_line(), line);
        }
    }

    #[test]
    fn a_line_without_its_fingerprint_still_reads() {
        let four: HistoryLine = "shared\tquickie\t1500\tsomeone".parse().unwrap();
        assert_eq!(four.agent.as_deref(), Some("someone"));
        assert_eq!((four.secs, four.fingerprint), (1500, None));
    }

    #[test]
    fn a_line_without_its_agent_still_reads() {
        let three: HistoryLine = "bench\tsweep\t60".parse().unwrap();
        assert_eq!((three.label, three.agent), (Label::new("sweep"), None));
    }

    #[test]
    fn a_line_of_too_few_fields_is_refused() {
        assert_eq!(
            "shared\tquickie".parse::<HistoryLine>(),
            Err(LineError::FieldCount {
                record: "history",
                found: 2
            })
        );
    }

    #[test]
    fn a_line_of_too_many_fields_is_refused() {
        assert_eq!(
            format!("{LINE}\tlater").parse::<HistoryLine>(),
            Err(LineError::FieldCount {
                record: "history",
                found: 6
            })
        );
    }

    #[test]
    fn each_field_that_does_not_read_is_refused_by_name() {
        for (index, field) in [(0, "mode"), (2, "seconds")] {
            assert_eq!(
                line_with_field(LINE, index, "?").parse::<HistoryLine>(),
                Err(LineError::Value {
                    field,
                    value: "?".into()
                })
            );
        }
    }
}
