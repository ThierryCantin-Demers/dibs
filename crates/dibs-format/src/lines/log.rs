use crate::{
    BatchId, JobId, Label, Mode,
    lines::base::{Dashed, Field, Fields, LineError},
};
use std::{fmt, str::FromStr};

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
}

impl Event {
    const ALL: [Event; 11] = [
        Event::Arrived,
        Event::Refused,
        Event::Cancelled,
        Event::Bypassed,
        Event::Peek,
        Event::PeekSlow,
        Event::Finished,
        Event::Killed,
        Event::Aborted,
        Event::CallerGone,
        Event::Reclaimed,
    ];

    pub fn as_str(self) -> &'static str {
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
        }
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Event {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Event::ALL
            .into_iter()
            .find(|e| e.as_str() == s)
            .ok_or_else(|| format!("no event is called {s:?}"))
    }
}

/// One line of a machine's log: what ran there, what it cost and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// As `date -Is` writes it, in the machine's own zone.
    pub when: String,
    pub event: Event,
    pub pid: u32,
    pub mode: Mode,
    pub label: Label,
    pub queued: Option<u64>,
    pub ran: Option<u64>,
    pub exit: Option<i32>,
    pub command: String,
    /// The agent, followed by ` on <card>` when the job named one.
    pub agent: String,
    pub batch: Option<BatchId>,
    pub job: Option<JobId>,
}

impl FromStr for LogLine {
    type Err = LineError;

    /// Reads the 12 fields written today, and the 10 and 11 from before a line named its batch
    /// and its job.
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut f = Fields::of(line);
        let found = f.count();
        if !(10..=12).contains(&found) {
            return Err(LineError::FieldCount {
                record: "log",
                found,
            });
        }
        Ok(LogLine {
            when: f.text().to_string(),
            event: f.parsed("event")?,
            pid: f.parsed("pid")?,
            mode: f.parsed("mode")?,
            label: f.parsed("label")?,
            queued: f.dashed("queued")?,
            ran: f.dashed("ran")?,
            exit: f.dashed("exit")?,
            command: f.text().to_string(),
            agent: f.text().to_string(),
            batch: match found >= 11 {
                true => f.dashed("batch")?,
                false => None,
            },
            job: match found >= 12 {
                true => f.dashed("job")?,
                false => None,
            },
        })
    }
}

impl fmt::Display for LogLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            Field(&self.when),
            self.event,
            self.pid,
            self.mode,
            Field(self.label.as_str()),
            Dashed(self.queued.as_ref()),
            Dashed(self.ran.as_ref()),
            Dashed(self.exit.as_ref()),
            Field(&self.command),
            Field(&self.agent),
            Dashed(self.batch.as_ref()),
            Dashed(self.job.as_ref()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINES: [&str; 6] = [
        "2026-10-01T12:00:00+02:00\tarrived\t4242\tshared\tapp_build_plain\t-\t-\t-\t# echo hi\tsession suite\t-\t20261001-120000-4242",
        "2026-10-01T12:00:03+02:00\tfinished\t4242\tshared\tapp_build_plain\t0\t3\t0\t# echo hi\tsession suite\t-\t20261001-120000-4242",
        "2026-10-01T12:00:04+02:00\tfinished\t4243\tbench\trec-bench\t1\t2\t3\texit 3\tsession suite on gpu0\t20261001-115959-77\t20261001-120004-4243",
        "2026-10-01T12:00:05+02:00\treclaimed\t4244\trelease\treclaim\t-\t-\t-\treclaimed the lock from orphan pid 9\t?\t-\t-",
        "2026-10-01T12:00:06+02:00\tpeek-slow\t4245\tpeek\tps\t-\t2\t0\tps aux\tsession suite\t-\t-",
        "2026-10-01T12:00:07+02:00\tcaller-gone\t4246\trsh\tsync\t5\t-\t-\trsync --server\tsession suite\t-\t-",
    ];

    #[test]
    fn the_lines_a_machine_writes_today_read_and_write_back_byte_for_byte() {
        for line in LINES {
            assert_eq!(line.parse::<LogLine>().unwrap().to_string(), line);
        }
    }

    #[test]
    fn a_finished_line_says_what_it_cost_and_where_it_belongs() {
        let line: LogLine = LINES[2].parse().unwrap();
        assert_eq!(line.event, Event::Finished);
        assert_eq!(
            (line.queued, line.ran, line.exit),
            (Some(1), Some(2), Some(3))
        );
        assert_eq!(line.batch, Some(BatchId::new("20261001-115959-77")));
        assert_eq!(line.agent, "session suite on gpu0");
    }

    #[test]
    fn an_older_line_reads_without_its_batch_or_job() {
        let ten: LogLine = "2026-09-01T00:00:00+00:00\tarrived\t1\tshared\tx\t-\t-\t-\tls\tsomeone"
            .parse()
            .unwrap();
        assert_eq!((ten.batch, ten.job), (None, None));
        let eleven: LogLine =
            "2026-09-01T00:00:00+00:00\tarrived\t1\tshared\tx\t-\t-\t-\tls\tsomeone\tb1"
                .parse()
                .unwrap();
        assert_eq!(eleven.batch, Some(BatchId::new("b1")));
    }

    #[test]
    fn a_line_of_no_known_shape_is_refused() {
        assert!("2026\tarrived\t1".parse::<LogLine>().is_err());
        assert!(
            LINES[0]
                .replace("arrived", "landed")
                .parse::<LogLine>()
                .is_err()
        );
    }
}
