use crate::{clock::Moment, shared::SharedFile, tree};
use dibs_format::{BatchId, Event, JobId, Label, LockRecord, LogLine, Mode, wire::Request};
use std::path::Path;

/// How much of a command its records keep.
pub const ONE_LINE: usize = 200;
const NAME: usize = 48;

/// A request, with what the machine derives from it before it acts.
#[derive(Debug, Clone)]
pub struct Call {
    pub request: Request,
    /// The caller's title on one line, `?` when it gave none.
    pub agent: String,
    pub agent_id: String,
    /// The first line of the batch plan: the batch's id, then the step.
    pub batch_tag: Option<String>,
    /// The command on one line and cut short, as the records and the log carry it.
    pub one_line: String,
    /// What the job runs: the request's command, or for `--gc` the sweep.
    pub work: String,
    /// This process, which the records name.
    pub pid: u32,
}

impl Call {
    pub fn of(request: Request) -> Call {
        let agent = request.agent.one_line(NAME);
        let agent_id = request.agent_id.one_line(NAME);
        let batch_tag = request
            .batch
            .as_deref()
            .filter(|b| !b.is_empty())
            .map(|b| b.lines().next().unwrap_or_default().replace('\t', " "));
        let mut command = request.command.one_line(ONE_LINE);
        if request.command.len() > ONE_LINE {
            command.push_str(" …");
        }
        if request.watch.hold {
            command = format!("held for a command run elsewhere: {command}");
        }
        let mut work = request.command.clone();
        if request.mode == Mode::Gc {
            let asked = tree::Asked::parse(&request.command);
            command = asked.named();
            work = asked.command();
        }
        Call {
            agent: match agent.is_empty() {
                true => "?".into(),
                false => agent,
            },
            agent_id,
            batch_tag,
            one_line: command,
            work,
            pid: std::process::id(),
            request,
        }
    }

    pub fn mode(&self) -> Mode {
        self.request.mode
    }

    pub fn label(&self) -> &Label {
        &self.request.label
    }

    /// The batch this call is a step of.
    pub fn batch_id(&self) -> Option<&str> {
        self.batch_tag
            .as_deref()
            .map(|tag| tag.split(' ').next().unwrap_or_default())
    }

    /// The record that names this call and its job in the lock directory, as of `start`.
    pub fn lock_record(&self, start: u64, job: &JobId) -> LockRecord {
        LockRecord {
            mode: self.mode(),
            pid: self.pid,
            start,
            label: self.label().clone(),
            agent: self.agent.clone(),
            agent_id: Some(self.agent_id.clone()).filter(|id| !id.is_empty()),
            device: self.request.card.as_ref().map(|c| c.alias.clone()),
            command: self.one_line.clone(),
            fingerprint: self.request.fingerprint.clone(),
            job: Some(job.clone()),
        }
    }

    /// A log line about this call, with nothing yet to say about time or exit.
    pub fn log_line(&self, event: Event) -> LogLine {
        let device = self
            .request
            .card
            .as_ref()
            .map(|c| format!(" on {}", c.alias))
            .unwrap_or_default();
        LogLine {
            when: Moment::now().iso(),
            event,
            pid: self.pid,
            mode: self.mode(),
            label: self.label().clone(),
            queued: None,
            ran: None,
            exit: None,
            command: match self.one_line.is_empty() {
                true => self.label().to_string(),
                false => self.one_line.clone(),
            },
            agent: format!("{}{device}", self.agent),
            batch: self.batch_tag.as_deref().map(BatchId::new),
            job: None,
        }
    }
}

/// Free text as a record's one field holds it.
pub trait OneLine {
    /// On one line, cut to at most `bytes` bytes on a character's boundary.
    fn one_line(&self, bytes: usize) -> String;
}

impl OneLine for str {
    fn one_line(&self, bytes: usize) -> String {
        let mut line = self.replace(['\n', '\t'], " ");
        let mut end = line.len().min(bytes);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line
    }
}

/// The machine's log: every arrival and every end, so a job that was killed or wedged still
/// leaves a trace.
pub struct Journal<'a> {
    pub path: &'a Path,
}

impl Journal<'_> {
    pub fn write(&self, line: &LogLine) {
        let _ = SharedFile { path: self.path }.append(&line.to_string());
    }

    /// Cut back to its last `kept` lines once it outgrows `bound`.
    pub fn trim(&self, bound: usize, kept: usize) {
        let _ = SharedFile { path: self.path }.rewrite(|text| {
            let lines: Vec<&str> = text.lines().collect();
            (lines.len() > bound).then(|| {
                lines[lines.len() - kept.min(lines.len())..]
                    .iter()
                    .map(|l| format!("{l}\n"))
                    .collect()
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_one_line_cut_on_a_character() {
        assert_eq!("a\tb\nc".one_line(10), "a b c");
        assert_eq!("é".one_line(1), "");
        assert_eq!("abcdef".one_line(3), "abc");
    }
}
