//! What is read of the machine without the lock: its status, a watch of it, and its log.

use crate::{channel::Channel, clock::Moment, sink::Sink, status::Look};
use dibs_format::{status::Status, wire::Request};
use std::{
    fs, thread,
    time::{Duration, Instant},
};

/// A watch redraws this often unless asked otherwise.
const EVERY: u64 = 5;
/// Log lines shown unless asked otherwise.
const LOG_LINES: usize = 40;
const CLEAR: &str = "\x1b[H\x1b[2J\x1b[3J";

/// The calls that only read the lock directory, the history and the log.
pub struct Views<'a> {
    pub look: Look<'a>,
    pub sink: &'a Sink,
    pub request: &'a Request,
}

impl Views<'_> {
    pub fn status(&self) -> i32 {
        let status = self.look.status(self.request.verbose);
        self.sink.out(self.shown(&status).as_bytes());
        0
    }

    /// The status as JSON on one line, or as text.
    fn shown(&self, status: &Status) -> String {
        match self.request.json {
            true => status.line(),
            false => self.look.text(status, self.request.tty),
        }
    }

    /// A redraw every interval, until the caller goes: the wait for the next tick is also the
    /// watch on the channel, and a caller's beat only says it is still there.
    pub fn watch(&self, channel: Option<Channel>) -> i32 {
        let every = self.request.label.as_str().parse().unwrap_or(EVERY).max(1);
        let watch = self.request.watch;
        let mut channel = channel.filter(|_| !watch.off);
        let mut heard = Instant::now();
        let mut present = true;
        while present {
            let status = self.look.status(self.request.verbose);
            let tick = match self.request.json {
                true => self.shown(&status),
                false => format!(
                    "{}{}   every {every}s, ctrl-c to stop\n{}",
                    if self.request.tty { CLEAR } else { "" },
                    Moment::now().clock(),
                    self.shown(&status)
                ),
            };
            let due = Instant::now() + Duration::from_secs(every);
            present = self.sink.out(tick.as_bytes())
                && match channel.as_mut() {
                    Some(channel) => channel.attend(due, watch.lease, &mut heard),
                    None => {
                        thread::sleep(Duration::from_secs(every));
                        true
                    }
                };
        }
        0
    }

    /// The last lines of the log, as a table.
    pub fn log(&self) -> i32 {
        let shown = self.request.label.as_str().parse().unwrap_or(LOG_LINES);
        let path = &self.look.machine.log;
        let text = fs::read_to_string(path).unwrap_or_default();
        if text.is_empty() {
            self.sink
                .out(format!("Nothing logged yet ({}).\n", path.display()).as_bytes());
            return 0;
        }
        let mut table = row([
            "WHEN", "EVENT", "MODE", "LABEL", "QUEUED", "RAN", "EXIT", "JOB", "AGENT", "COMMAND",
        ]);
        let lines: Vec<&str> = text.lines().collect();
        for line in &lines[lines.len().saturating_sub(shown)..] {
            table.push_str(&LogRow(line).to_string());
        }
        self.sink.out(table.as_bytes());
        0
    }
}

/// One line of the log as the table shows it. Lines written before events carried a job id name
/// the process instead.
struct LogRow<'a>(&'a str);

impl std::fmt::Display for LogRow<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let fields: Vec<&str> = self.0.split('\t').collect();
        let field = |n: usize| fields.get(n - 1).copied().unwrap_or_default();
        let cut = |text: &str, at: usize| text.chars().take(at).collect::<String>();
        let job = match field(12) {
            "" | "-" => format!("pid {}", field(3)),
            job => job.to_string(),
        };
        let agent = match field(10) {
            "" => "?",
            agent => agent,
        };
        let batch = match field(11) {
            "" | "-" => String::new(),
            batch => format!("  [batch {batch}]"),
        };
        let mut said = row([
            &cut(field(1), 19),
            field(2),
            field(4),
            &cut(field(5), 14),
            field(6),
            field(7),
            field(8),
            &job,
            &cut(agent, 22),
            &cut(field(9), 50),
        ]);
        said.insert_str(said.len() - 1, &batch);
        f.write_str(&said)
    }
}

/// The table's columns, padded as the log has always printed them.
fn row(c: [&str; 10]) -> String {
    format!(
        "{:<19} {:<9} {:<7} {:<14} {:>6} {:>6} {:>5}  {:<21}  {:<22} {}\n",
        c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7], c[8], c[9]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_line_reads_as_awk_printed_it() {
        let line = "2026-09-02T11:03:00+02:00\tfinished\t1002\tbench\tgemm\t5\t175\t0\tcargo bench\tan agent on gpu:card\t20260902-110000-7 measure\t-";
        assert_eq!(
            LogRow(line).to_string(),
            "2026-09-02T11:03:00 finished  bench   gemm                5    175     0  pid 1002               an agent on gpu:card   cargo bench  [batch 20260902-110000-7 measure]\n"
        );
    }
}
