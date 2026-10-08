use crate::{
    Label, Mode,
    lines::base::{Field, Fields, LineError},
};
use std::{fmt, str::FromStr};

/// One successful run in a machine's duration history, which every estimate is drawn from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryLine {
    pub mode: Mode,
    pub label: Label,
    pub seconds: u64,
    pub agent: Option<String>,
    pub fingerprint: Option<String>,
}

impl FromStr for HistoryLine {
    type Err = LineError;

    /// Reads the 5 fields written today, and 3 or 4 from a line with no agent or fingerprint.
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut f = Fields::of(line);
        let found = f.count();
        if !(3..=5).contains(&found) {
            return Err(LineError::FieldCount {
                record: "history",
                found,
            });
        }
        Ok(HistoryLine {
            mode: f.parsed("mode")?,
            label: f.parsed("label")?,
            seconds: f.parsed("seconds")?,
            agent: f.optional(),
            fingerprint: f.optional(),
        })
    }
}

impl fmt::Display for HistoryLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\t{}\t{}\t{}\t{}",
            self.mode,
            Field(self.label.as_str()),
            self.seconds,
            Field(self.agent.as_deref().unwrap_or_default()),
            Field(self.fingerprint.as_deref().unwrap_or_default()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lines_a_machine_writes_today_read_and_write_back_byte_for_byte() {
        for line in [
            "shared\tapp_build_plain\t12\tsession suite\te8e800432f19a324",
            "shared\tquickie\t3\tsession suite\t",
            "bench\trec-queued-bench\t40\tsession suite\t",
        ] {
            assert_eq!(line.parse::<HistoryLine>().unwrap().to_string(), line);
        }
    }

    #[test]
    fn a_line_without_its_fingerprint_or_agent_still_reads() {
        let four: HistoryLine = "shared\tquickie\t1500\tsomeone".parse().unwrap();
        assert_eq!(four.agent.as_deref(), Some("someone"));
        assert_eq!((four.seconds, four.fingerprint), (1500, None));
        let three: HistoryLine = "bench\tsweep\t60".parse().unwrap();
        assert_eq!((three.label, three.agent), (Label::new("sweep"), None));
    }

    #[test]
    fn a_line_of_no_known_shape_is_refused() {
        assert!("shared\tquickie".parse::<HistoryLine>().is_err());
        assert!("shared\tquickie\tlong".parse::<HistoryLine>().is_err());
    }
}
