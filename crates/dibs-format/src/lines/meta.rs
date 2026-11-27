use crate::{
    Label, Mode,
    lines::{
        error::LineError,
        field::Field,
        reader::{non_empty, parse_field},
    },
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, str::FromStr};

/// Who produced a job's exit: the command, or dibs stopping it.
///
/// # Guarantees
///
/// - `from_str` reads back every name `Display` writes, and refuses any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExitSource {
    Command,
    Dibs,
}

impl ExitSource {
    const ALL: [ExitSource; 2] = [ExitSource::Command, ExitSource::Dibs];

    fn as_str(self) -> &'static str {
        match self {
            ExitSource::Command => "command",
            ExitSource::Dibs => "dibs",
        }
    }
}

impl fmt::Display for ExitSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ExitSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ExitSource::ALL
            .into_iter()
            .find(|source| source.as_str() == s)
            .ok_or_else(|| format!("an exit is by the command or dibs, not {s:?}"))
    }
}

/// How a job ended: the `meta` file in its directory, one `key<TAB>value` line each.
///
/// # Guarantees
///
/// - Writes the file a machine writes today, byte for byte, with a `user` line only when there
///   is a user who is not empty, and reads it with its `who` or `user` line absent or empty as
///   none.
/// - Free text never adds a field or a line: a tab, a newline or a carriage return in it is
///   written as a space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobMeta {
    pub mode: Mode,
    pub label: Label,
    pub queued_secs: u64,
    pub ran_secs: u64,
    pub exit: i32,
    pub by: ExitSource,
    pub agent: String,
    /// How many lines its log holds.
    pub line_count: u64,
    /// The session that ran it, which `dibs --kill` asks of what it left running; absent in a
    /// job that ended before it was kept.
    pub who: Option<String>,
    /// The person the caller ran as on its own computer, absent when it named none.
    pub user: Option<String>,
}

impl JobMeta {
    /// The text of the `meta` file, each line ending in a newline.
    pub fn to_text(&self) -> String {
        let user = self
            .user
            .as_deref()
            .and_then(non_empty)
            .map(|user| format!("user\t{}\n", Field::from_text(&user)))
            .unwrap_or_default();
        format!(
            "mode\t{}\nlabel\t{}\nqueued\t{}\nran\t{}\nexit\t{}\nby\t{}\nagent\t{}\nlines\t{}\nwho\t{}\n{user}",
            self.mode,
            Field::from_text(self.label.as_str()),
            self.queued_secs,
            self.ran_secs,
            self.exit,
            self.by,
            Field::from_text(&self.agent),
            self.line_count,
            Field::from_optional(self.who.as_deref()),
        )
    }
}

impl FromStr for JobMeta {
    type Err = LineError;

    /// # Errors
    ///
    /// [`LineError::Missing`] for a file without its mode, label, queued, ran, exit, by, agent or
    /// lines, and [`LineError::Value`] for a mode, a queued, a ran, an exit, a by or a lines that
    /// does not read.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let values: BTreeMap<&str, &str> = text
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .collect();
        let required = |field: &'static str| {
            values
                .get(field)
                .copied()
                .ok_or(LineError::Missing { field })
        };
        let optional = |field: &str| values.get(field).copied().and_then(non_empty);
        Ok(JobMeta {
            mode: parse_field("mode", required("mode")?)?,
            label: Label::new(required("label")?),
            queued_secs: parse_field("queued", required("queued")?)?,
            ran_secs: parse_field("ran", required("ran")?)?,
            exit: parse_field("exit", required("exit")?)?,
            by: parse_field("by", required("by")?)?,
            agent: required("agent")?.to_string(),
            line_count: parse_field("lines", required("lines")?)?,
            who: optional("who"),
            user: optional("user"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &str = "mode\tshared\nlabel\trec-plain\nqueued\t0\nran\t1\nexit\t0\nby\tcommand\nagent\tsession suite\nlines\t2\nwho\tlocal_suite\n";
    const SERVICE_FAILED: &str = "mode\tshared\nlabel\trec-with\nqueued\t0\nran\t1\nexit\t77\nby\tdibs\nagent\tsession suite\nlines\t0\nwho\t\n";
    const FOR_A_USER: &str = "mode\tshared\nlabel\trec-plain\nqueued\t0\nran\t1\nexit\t0\nby\tcommand\nagent\tsession suite\nlines\t2\nwho\tlocal_suite\nuser\tthierry\n";

    #[test]
    fn the_files_a_machine_writes_today_read_and_write_back_byte_for_byte() {
        for text in [PLAIN, SERVICE_FAILED, FOR_A_USER] {
            assert_eq!(text.parse::<JobMeta>().unwrap().to_text(), text);
        }
    }

    #[test]
    fn an_empty_user_writes_no_user_line() {
        let meta = JobMeta {
            user: Some(String::new()),
            ..PLAIN.parse().unwrap()
        };
        assert_eq!(meta.to_text(), PLAIN);
    }

    #[test]
    fn an_empty_who_reads_as_nobodys() {
        assert_eq!(SERVICE_FAILED.parse::<JobMeta>().unwrap().who, None);
    }

    #[test]
    fn a_file_kept_before_jobs_named_their_session_reads_as_nobodys() {
        let before = SERVICE_FAILED.replace("who\t\n", "");
        assert_eq!(before.parse::<JobMeta>().unwrap().who, None);
    }

    #[test]
    fn a_file_missing_a_required_field_is_refused_by_name() {
        for field in [
            "mode", "label", "queued", "ran", "exit", "by", "agent", "lines",
        ] {
            let without: String = PLAIN
                .lines()
                .filter(|line| !line.starts_with(&format!("{field}\t")))
                .map(|line| format!("{line}\n"))
                .collect();
            assert_eq!(
                without.parse::<JobMeta>(),
                Err(LineError::Missing { field })
            );
        }
    }

    #[test]
    fn each_field_that_does_not_read_is_refused_by_name() {
        for field in ["mode", "queued", "ran", "exit", "by", "lines"] {
            let corrupted: String = PLAIN
                .lines()
                .map(|line| match line.split_once('\t') {
                    Some((key, _)) if key == field => format!("{key}\t?\n"),
                    _ => format!("{line}\n"),
                })
                .collect();
            assert_eq!(
                corrupted.parse::<JobMeta>(),
                Err(LineError::Value {
                    field,
                    value: "?".into()
                })
            );
        }
    }

    #[test]
    fn every_source_reads_back_from_its_name() {
        for source in ExitSource::ALL {
            assert_eq!(source.as_str().parse::<ExitSource>(), Ok(source));
        }
    }

    #[test]
    fn a_name_no_source_has_is_refused() {
        assert!("someone".parse::<ExitSource>().is_err());
    }

    /// Stops compiling when a variant is added, until it is counted here and listed in `ALL`.
    #[test]
    fn every_variant_is_in_all() {
        let counted = |source: ExitSource| match source {
            ExitSource::Command | ExitSource::Dibs => 1,
        };
        assert_eq!(ExitSource::ALL.into_iter().map(counted).sum::<usize>(), 2);
    }
}
