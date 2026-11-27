use crate::{JobId, Label, Mode, lines::field::Field};
use std::fmt;

/// What happened to a job, as a machine's log names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    Arrived,
    Refused,
    Cancelled,
    Bypassed,
    Peek,
    PeekSlow,
    Finished,
    Killed,
    Aborted,
    CallerGone,
    Reclaimed,
    HandedOver,
}

impl Event {
    fn as_str(self) -> &'static str {
        match self {
            Event::Arrived => "arrived",
            Event::Refused => "refused",
            Event::Cancelled => "cancelled",
            Event::Bypassed => "bypassed",
            Event::Peek => "peek",
            Event::PeekSlow => "peek-slow",
            Event::Finished => "finished",
            Event::Killed => "killed",
            Event::Aborted => "aborted",
            Event::CallerGone => "caller-gone",
            Event::Reclaimed => "reclaimed",
            Event::HandedOver => "handed-over",
        }
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One line of a machine's log: what ran there, what it cost and how it ended.
///
/// # Guarantees
///
/// - Writes the 12 fields a machine writes today, byte for byte.
/// - Free text never adds a field or a line: a tab, a newline or a carriage return in it is
///   written as a space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// As `date -Is` writes it, in the machine's own zone.
    pub when: String,
    pub event: Event,
    pub pid: u32,
    pub mode: Mode,
    pub label: Label,
    pub queued_secs: Option<u64>,
    pub ran_secs: Option<u64>,
    pub exit: Option<i32>,
    pub command: String,
    /// The agent, followed by ` on <card>` when the job named one.
    pub agent: String,
    /// The batch the job is a step of, as `<id> <step>`: the first line of the plan it carried.
    pub batch_tag: Option<String>,
    pub job: Option<JobId>,
}

impl LogLine {
    /// The line the log holds for this event, without its newline.
    pub fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            Field::from_text(&self.when),
            self.event,
            self.pid,
            self.mode,
            Field::from_text(self.label.as_str()),
            Field::from_dashed(self.queued_secs.as_ref()),
            Field::from_dashed(self.ran_secs.as_ref()),
            Field::from_dashed(self.exit.as_ref()),
            Field::from_text(&self.command),
            Field::from_text(&self.agent),
            Field::from_dashed(self.batch_tag.as_ref()),
            Field::from_dashed(self.job.as_ref()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrived() -> LogLine {
        LogLine {
            when: "2026-10-01T12:00:00+02:00".into(),
            event: Event::Arrived,
            pid: 4242,
            mode: Mode::Shared,
            label: Label::new("app_build_plain"),
            queued_secs: None,
            ran_secs: None,
            exit: None,
            command: "# echo hi".into(),
            agent: "session suite".into(),
            batch_tag: None,
            job: Some(JobId::new("20261001-120000-4242")),
        }
    }

    #[test]
    fn an_arrival_is_written_with_dashes_for_what_it_cannot_say_yet() {
        assert_eq!(
            arrived().to_line(),
            "2026-10-01T12:00:00+02:00\tarrived\t4242\tshared\tapp_build_plain\t-\t-\t-\t# echo hi\tsession suite\t-\t20261001-120000-4242"
        );
    }

    #[test]
    fn an_end_is_written_with_what_it_cost_and_its_batch() {
        let finished = LogLine {
            event: Event::Finished,
            queued_secs: Some(1),
            ran_secs: Some(2),
            exit: Some(3),
            batch_tag: Some("20261001-115959-77 build".into()),
            ..arrived()
        };
        assert_eq!(
            finished.to_line(),
            "2026-10-01T12:00:00+02:00\tfinished\t4242\tshared\tapp_build_plain\t1\t2\t3\t# echo hi\tsession suite\t20261001-115959-77 build\t20261001-120000-4242"
        );
    }

    #[test]
    fn a_line_with_no_job_is_written_with_a_dash_for_it() {
        let reclaimed = LogLine {
            event: Event::Reclaimed,
            job: None,
            ..arrived()
        };
        assert!(reclaimed.to_line().ends_with("\t-\t-"));
    }
}
