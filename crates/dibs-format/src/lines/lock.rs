use crate::{
    Alias, Label, Mode,
    lines::base::{Dashed, Field, Fields, LineError},
};
use std::{fmt, str::FromStr};

/// A job holding the lock or waiting for it: the line in `holder.<pid>` and `waiting.<pid>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRecord {
    pub mode: Mode,
    pub pid: u32,
    /// When it arrived, for a waiter, or took the lock, for a holder: seconds since the epoch.
    pub start: u64,
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
}

impl FromStr for LockRecord {
    type Err = LineError;

    /// Reads the 9 fields written today, and the 6, 7 and 8 an older machine half wrote: one
    /// without the agent's id, then without the card, then without the fingerprint.
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut f = Fields::of(line);
        let found = f.count();
        if !(6..=9).contains(&found) {
            return Err(LineError::FieldCount {
                record: "lock",
                found,
            });
        }
        Ok(LockRecord {
            mode: f.parsed("mode")?,
            pid: f.parsed("pid")?,
            start: f.parsed("start")?,
            label: f.parsed("label")?,
            agent: f.text().to_string(),
            agent_id: (found >= 7).then(|| f.optional()).flatten(),
            device: match found >= 8 {
                true => f.dashed("device")?,
                false => None,
            },
            command: f.text().to_string(),
            fingerprint: f.optional(),
        })
    }
}

impl fmt::Display for LockRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.mode,
            self.pid,
            self.start,
            Field(self.label.as_str()),
            Field(&self.agent),
            Field(self.agent_id.as_deref().unwrap_or_default()),
            Dashed(self.device.as_ref()),
            Field(&self.command),
            Field(self.fingerprint.as_deref().unwrap_or_default()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECIPE_HOLDER: &str = "bench\t4242\t1759300000\tapp_bench_held_cpu\tsession suite\tlocal_suite\tcpu\tcd /r/scratch/ws/app/t && export CARGO_TARGET_DIR=/r/scratch/target/app && { echo \"DIBS-STATE ${DIBS_STATE:-}\" read -r _ < \"$REC_GATE\"; }\t21eafeca56a1b2ba";
    const PLAIN_WAITER: &str = "shared\t4243\t1759300001\trec-served\tsession suite\tlocal_suite\t-\techo up > /r/f-up; read -r _ < /r/f-served\t";
    const HOLD: &str = "shared\t4244\t1759300002\trec-hold\tsession suite\tlocal_suite\t-\theld for a command run elsewhere: read -r _ < /r/f-held\t";

    #[test]
    fn the_lines_a_machine_writes_today_read_and_write_back_byte_for_byte() {
        for line in [RECIPE_HOLDER, PLAIN_WAITER, HOLD] {
            let record: LockRecord = line.parse().unwrap();
            assert_eq!(record.to_string(), line);
        }
    }

    #[test]
    fn a_recipe_holder_names_its_card_and_fingerprint() {
        let record: LockRecord = RECIPE_HOLDER.parse().unwrap();
        assert_eq!(record.mode, Mode::Bench);
        assert_eq!(record.pid, 4242);
        assert_eq!(record.device, Some(Alias::new("cpu")));
        assert_eq!(record.fingerprint.as_deref(), Some("21eafeca56a1b2ba"));
        let plain: LockRecord = PLAIN_WAITER.parse().unwrap();
        assert_eq!((plain.device, plain.fingerprint), (None, None));
    }

    #[test]
    fn an_older_holder_reads_with_what_it_has() {
        let six: LockRecord = "shared\t7\t100\tsix-fields\tan older dibs\tits command"
            .parse()
            .unwrap();
        assert_eq!(six.agent, "an older dibs");
        assert_eq!(six.agent_id, None);
        assert_eq!(six.command, "its command");
        let seven: LockRecord = "shared\t7\t100\tl\tagent\tid\tcmd".parse().unwrap();
        assert_eq!(
            (seven.agent_id.as_deref(), seven.command.as_str()),
            (Some("id"), "cmd")
        );
        let eight: LockRecord = "bench\t7\t100\tl\tagent\tid\tgpu0\tcmd".parse().unwrap();
        assert_eq!(eight.device, Some(Alias::new("gpu0")));
        assert_eq!((eight.command.as_str(), eight.fingerprint), ("cmd", None));
    }

    #[test]
    fn a_line_of_no_known_shape_is_refused() {
        assert_eq!(
            "shared\t7\t100".parse::<LockRecord>(),
            Err(LineError::FieldCount {
                record: "lock",
                found: 3
            })
        );
        assert!("held\t7\t100\tl\ta\tc".parse::<LockRecord>().is_err());
    }

    #[test]
    fn free_text_cannot_break_the_line() {
        let mut record: LockRecord = PLAIN_WAITER.parse().unwrap();
        record.command = "a\tb\nc".into();
        assert_eq!(record.to_string().split('\t').count(), 9);
        assert!(!record.to_string().contains('\n'));
    }
}
