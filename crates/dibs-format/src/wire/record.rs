use crate::{By, JobId, Label, Mode};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A fact a runner tells its client besides the output it passes on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Record {
    /// How a job ended, which the client prints on stderr.
    Trailer(Trailer),
    /// The lock is held for a command the client runs, with the ports picked for it.
    Holding(Vec<Picked>),
}

/// A `--port` name and the port the machine picked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Picked {
    pub name: String,
    pub port: u16,
}

/// The one line a job ends with, the same shape every time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trailer {
    pub job: JobId,
    pub mode: Mode,
    pub label: Label,
    pub queued: u64,
    pub ran: u64,
    pub exit: i32,
    pub by: By,
    /// What cargo compiled, when the log shows cargo finishing.
    pub built: Option<Built>,
}

/// How many crates a job's cargo compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Built {
    Crates(u64),
    Nothing,
}

impl fmt::Display for Trailer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "job {}  {}  {}  queued {}s  ran {}s  exit {}  by={}",
            self.job, self.mode, self.label, self.queued, self.ran, self.exit, self.by
        )?;
        match self.built {
            Some(Built::Crates(n)) => write!(f, "  built={n}"),
            Some(Built::Nothing) => f.write_str("  built=nothing"),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailer_reads_as_a_machine_has_always_printed_it() {
        let mut trailer = Trailer {
            job: JobId::new("20261001-120000-4242"),
            mode: Mode::Shared,
            label: Label::new("app_build"),
            queued: 0,
            ran: 3,
            exit: 0,
            by: By::Command,
            built: None,
        };
        assert_eq!(
            trailer.to_string(),
            "job 20261001-120000-4242  shared  app_build  queued 0s  ran 3s  exit 0  by=command"
        );
        trailer.built = Some(Built::Nothing);
        assert!(trailer.to_string().ends_with("by=command  built=nothing"));
        trailer.built = Some(Built::Crates(12));
        assert!(trailer.to_string().ends_with("by=command  built=12"));
    }
}
