use crate::{
    Label, Mode,
    lines::base::{Field, LineError},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, str::FromStr};

/// Who produced a job's exit: the command, or dibs stopping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum By {
    Command,
    Dibs,
}

impl By {
    pub fn as_str(self) -> &'static str {
        match self {
            By::Command => "command",
            By::Dibs => "dibs",
        }
    }
}

impl fmt::Display for By {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for By {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "command" => Ok(By::Command),
            "dibs" => Ok(By::Dibs),
            other => Err(format!("an exit is by the command or dibs, not {other:?}")),
        }
    }
}

/// How a job ended: the `meta` file in its directory, one `key<TAB>value` line each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobMeta {
    pub mode: Mode,
    pub label: Label,
    pub queued: u64,
    pub ran: u64,
    pub exit: i32,
    pub by: By,
    pub agent: String,
    /// How many lines its log holds.
    pub lines: u64,
    /// The session that ran it, which `dibs --kill` asks of what it left running; absent in a
    /// job that ended before it was kept.
    pub who: Option<String>,
}

impl FromStr for JobMeta {
    type Err = LineError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let values: BTreeMap<&str, &str> = text
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .collect();
        let text = |field: &'static str| {
            values
                .get(field)
                .copied()
                .ok_or(LineError::Missing { field })
        };
        Ok(JobMeta {
            mode: LineError::parse("mode", text("mode")?)?,
            label: Label::new(text("label")?),
            queued: LineError::parse("queued", text("queued")?)?,
            ran: LineError::parse("ran", text("ran")?)?,
            exit: LineError::parse("exit", text("exit")?)?,
            by: LineError::parse("by", text("by")?)?,
            agent: text("agent")?.to_string(),
            lines: LineError::parse("lines", text("lines")?)?,
            who: values
                .get("who")
                .filter(|who| !who.is_empty())
                .map(|who| who.to_string()),
        })
    }
}

impl fmt::Display for JobMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "mode\t{}", self.mode)?;
        writeln!(f, "label\t{}", Field(self.label.as_str()))?;
        writeln!(f, "queued\t{}", self.queued)?;
        writeln!(f, "ran\t{}", self.ran)?;
        writeln!(f, "exit\t{}", self.exit)?;
        writeln!(f, "by\t{}", self.by)?;
        writeln!(f, "agent\t{}", Field(&self.agent))?;
        writeln!(f, "lines\t{}", self.lines)?;
        writeln!(f, "who\t{}", Field(self.who.as_deref().unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVICE_FAILED: &str = "mode\tshared\nlabel\trec-with\nqueued\t0\nran\t1\nexit\t77\nby\tdibs\nagent\tsession suite\nlines\t0\nwho\t\n";

    #[test]
    fn the_file_a_machine_writes_today_reads_and_writes_back_byte_for_byte() {
        let plain = "mode\tshared\nlabel\trec-plain\nqueued\t0\nran\t1\nexit\t0\nby\tcommand\nagent\tsession suite\nlines\t2\nwho\tlocal_suite\n";
        for text in [plain, SERVICE_FAILED] {
            assert_eq!(text.parse::<JobMeta>().unwrap().to_string(), text);
        }
        let meta: JobMeta = plain.parse().unwrap();
        assert_eq!(meta.who.as_deref(), Some("local_suite"));
    }

    #[test]
    fn a_file_kept_before_jobs_named_their_session_reads_as_nobodys() {
        let before = SERVICE_FAILED.replace("who\t\n", "");
        assert_eq!(before.parse::<JobMeta>().unwrap().who, None);
    }

    #[test]
    fn an_exit_dibs_gave_says_so() {
        let meta: JobMeta = SERVICE_FAILED.parse().unwrap();
        assert_eq!((meta.exit, meta.by), (77, By::Dibs));
    }

    #[test]
    fn a_file_missing_a_field_is_refused() {
        let without = SERVICE_FAILED.replace("lines\t0\n", "");
        assert_eq!(
            without.parse::<JobMeta>(),
            Err(LineError::Missing { field: "lines" })
        );
    }
}
