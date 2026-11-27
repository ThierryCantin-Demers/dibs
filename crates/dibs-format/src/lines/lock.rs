use crate::{
    Alias, JobId, Label, Mode,
    lines::{error::LineError, field::Field, reader::FieldReader},
};
use std::str::FromStr;

/// A job holding the lock or waiting for it: the line in `holder.<pid>` and `waiting.<pid>`.
///
/// # Assumptions
///
/// - The label is not empty.
///
/// # Guarantees
///
/// - Writes the 10 fields a runner writes today, byte for byte, or 9 for a record with no job.
/// - Reads those, and the 8, 7 and 6 a bash machine half wrote: without the fingerprint, then
///   the card, then the agent's id.
/// - Fields past the 10th are left unread rather than refused, since a runner older than this one
///   drops a record it cannot read, and with it a holder: what a call adds goes in a file beside
///   it.
/// - A field with nothing to say is written `-` wherever another follows it: bash reads these
///   lines splitting on tabs, which takes two in a row for one and moves every later field along.
///   So a literal `-` in the agent, its id, the command or the fingerprint reads back as empty
///   or none.
/// - Free text never adds a field or a line: a tab, a newline or a carriage return in it is
///   written as a space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRecord {
    pub mode: Mode,
    pub pid: u32,
    /// When it arrived, for a waiter, or took the lock, for a holder: seconds since the epoch.
    pub start_secs: u64,
    pub label: Label,
    /// The session's title, which a person asks about the job by.
    pub agent: String,
    /// The session's id, which ownership is decided by.
    pub agent_id: Option<String>,
    pub device: Option<Alias>,
    /// The command on one line, cut short.
    pub command: String,
    /// What the job's estimate is keyed by beside its label, for a recipe's job.
    pub fingerprint: Option<String>,
    /// The job's directory, which `dibs out <job>` reads; a bash machine half names none, and a
    /// field that is no job's id reads as none.
    pub job: Option<JobId>,
}

impl LockRecord {
    /// The line a lock record file holds for this job, without a newline.
    pub fn to_line(&self) -> String {
        let head = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
            self.mode,
            self.pid,
            self.start_secs,
            Field::from_text(self.label.as_str()),
            Field::from_filled(&self.agent),
            Field::from_filled_optional(self.agent_id.as_deref()),
            Field::from_dashed(self.device.as_ref()),
            Field::from_filled(&self.command),
        );
        match &self.job {
            Some(job) => format!(
                "{head}{}\t{}",
                Field::from_filled_optional(self.fingerprint.as_deref()),
                Field::from_text(job.as_str())
            ),
            None => format!(
                "{head}{}",
                Field::from_optional(self.fingerprint.as_deref())
            ),
        }
    }
}

impl FromStr for LockRecord {
    type Err = LineError;

    /// # Errors
    ///
    /// [`LineError::FieldCount`] for fewer than 6 fields, and [`LineError::Value`] for a mode, a
    /// pid or a start that does not read.
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut f = FieldReader::of_record(line, "lock", 6..)?;
        Ok(LockRecord {
            mode: f.read_parsed("mode")?,
            pid: f.read_parsed("pid")?,
            start_secs: f.read_parsed("start")?,
            label: Label::new(f.read_text()),
            agent: f.read_filled().unwrap_or_default(),
            agent_id: f.read_filled_if_fields_at_least(7),
            device: f.read_dashed_if_fields_at_least("device", 8)?,
            command: f.read_filled().unwrap_or_default(),
            fingerprint: f.read_filled(),
            job: f.read_job_id(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::reader::line_with_field;

    const RECIPE_HOLDER: &str = "bench\t4242\t1759300000\tapp_bench_held_cpu\tsession suite\tlocal_suite\tcpu\tcd /r/scratch/ws/app/t && export CARGO_TARGET_DIR=/r/scratch/target/app && { echo \"DIBS-STATE ${DIBS_STATE:-}\" read -r _ < \"$REC_GATE\"; }\t21eafeca56a1b2ba";
    const PLAIN_WAITER: &str = "shared\t4243\t1759300001\trec-served\tsession suite\tlocal_suite\t-\techo up > /r/f-up; read -r _ < /r/f-served\t";
    const HOLD: &str = "shared\t4244\t1759300002\trec-hold\tsession suite\tlocal_suite\t-\theld for a command run elsewhere: read -r _ < /r/f-held\t";

    /// A runner's record with nothing to say in any field that may say nothing.
    fn bare() -> LockRecord {
        LockRecord {
            mode: Mode::Shared,
            pid: 7,
            start_secs: 100,
            label: Label::new("dibs-runner"),
            agent: String::new(),
            agent_id: None,
            device: None,
            command: String::new(),
            fingerprint: None,
            job: Some(JobId::new("20261001120000-7")),
        }
    }

    #[test]
    fn the_lines_a_machine_writes_today_read_and_write_back_byte_for_byte() {
        for line in [RECIPE_HOLDER, PLAIN_WAITER, HOLD] {
            let record: LockRecord = line.parse().unwrap();
            assert_eq!(record.to_line(), line);
        }
    }

    #[test]
    fn a_recipe_holder_names_its_card_and_fingerprint() {
        let record: LockRecord = RECIPE_HOLDER.parse().unwrap();
        assert_eq!((record.mode, record.pid), (Mode::Bench, 4242));
        assert_eq!(record.device, Some(Alias::new("cpu")));
        assert_eq!(record.fingerprint.as_deref(), Some("21eafeca56a1b2ba"));
    }

    #[test]
    fn a_plain_waiter_names_no_card_or_fingerprint() {
        let plain: LockRecord = PLAIN_WAITER.parse().unwrap();
        assert_eq!((plain.device, plain.fingerprint), (None, None));
    }

    #[test]
    fn a_six_field_holder_reads_without_its_agents_id() {
        let six: LockRecord = "shared\t7\t100\tsix-fields\tan older dibs\tits command"
            .parse()
            .unwrap();
        assert_eq!(six.agent, "an older dibs");
        assert_eq!(six.agent_id, None);
        assert_eq!(six.command, "its command");
    }

    #[test]
    fn a_seven_field_holder_reads_its_agents_id_and_command() {
        let seven: LockRecord = "shared\t7\t100\tl\tagent\tid\tcmd".parse().unwrap();
        assert_eq!(
            (seven.agent_id.as_deref(), seven.command.as_str()),
            (Some("id"), "cmd")
        );
    }

    #[test]
    fn an_eight_field_holder_reads_its_card_without_a_fingerprint() {
        let eight: LockRecord = "bench\t7\t100\tl\tagent\tid\tgpu0\tcmd".parse().unwrap();
        assert_eq!(eight.device, Some(Alias::new("gpu0")));
        assert_eq!((eight.command.as_str(), eight.fingerprint), ("cmd", None));
    }

    #[test]
    fn a_runners_record_names_its_job_after_the_nine_a_bash_reader_knows() {
        let line = format!("{PLAIN_WAITER}\t20261001120000-4243");
        let record: LockRecord = line.parse().unwrap();
        assert_eq!(record.job, Some(JobId::new("20261001120000-4243")));
        assert_eq!(record.fingerprint, None);
    }

    #[test]
    fn a_nine_field_record_names_no_job() {
        let nine: LockRecord = PLAIN_WAITER.parse().unwrap();
        assert_eq!(nine.job, None);
    }

    #[test]
    fn a_record_reads_past_fields_it_does_not_know() {
        let line = format!("{PLAIN_WAITER}-\t20261001120000-4243");
        let record: LockRecord = line.parse().unwrap();
        let later: LockRecord = format!("{line}\tthierry\tsomething later").parse().unwrap();
        assert_eq!(later, record);
    }

    #[test]
    fn no_field_is_empty_where_another_follows_it() {
        let line = bare().to_line();
        assert!(!line.contains("\t\t"), "{line:?}");
    }

    #[test]
    fn an_empty_agent_and_no_agents_id_read_back_as_written() {
        let record = LockRecord {
            command: "cmd".into(),
            ..bare()
        };
        assert_eq!(record.to_line().parse::<LockRecord>().unwrap(), record);
    }

    #[test]
    fn an_empty_command_reads_back_empty() {
        let record = LockRecord {
            agent: "agent".into(),
            ..bare()
        };
        assert_eq!(record.to_line().parse::<LockRecord>().unwrap(), record);
    }

    #[test]
    fn a_line_of_too_few_fields_is_refused() {
        assert_eq!(
            "shared\t7\t100".parse::<LockRecord>(),
            Err(LineError::FieldCount {
                record: "lock",
                found: 3
            })
        );
    }

    #[test]
    fn each_field_that_does_not_read_is_refused_by_name() {
        for (index, field) in [(0, "mode"), (1, "pid"), (2, "start")] {
            assert_eq!(
                line_with_field(PLAIN_WAITER, index, "?").parse::<LockRecord>(),
                Err(LineError::Value {
                    field,
                    value: "?".into()
                })
            );
        }
    }
}
