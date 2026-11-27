use crate::{
    ExitSource, JobId, Label, Mode,
    wire::{PrepareReport, StepReport},
};
use serde::{Deserialize, Serialize};

/// A fact a runner tells its client besides the output it passes on.
///
/// Its JSON is stored: a client appends each record it hears to the file `$DIBS_TRAILERS` names,
/// which dibspatcher reads back.
///
/// # Guarantees
///
/// - Reads the JSON every released runner wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Record {
    /// How a job ended, which the client prints on stderr.
    Trailer(Trailer),
    /// The lock is held for a command the client runs, with the ports picked for it.
    Holding(Vec<PickedPort>),
    /// A transfer's request was read: from here on both streams are rsync's, raw, and the call's
    /// exit is the runner's own.
    Transferring,
    /// The job's tree is laid out, ahead of its command.
    Prepared(Box<PrepareReport>),
    /// What was done around a recipe step's command, before its trailer.
    Stepped(StepReport),
    /// The job ran past its check-in and goes on without the call, which ends 80.
    CheckedIn(CheckInReport),
    /// The request named an attempt the machine had already admitted, so nothing ran.
    Admitted(Admission),
}

/// How an attempt admitted before has gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Admission {
    /// It ended, as this trailer says.
    Ended(Trailer),
    /// Its job still runs, under this runner.
    Running { job: JobId, pid: u32 },
    /// Its runner is gone without a report. `started` once it held the lock, after which how
    /// it went cannot be known; before, it never ran.
    Lost { job: JobId, started: bool },
    /// Asked after only, and never admitted here.
    Never,
}

impl Admission {
    /// The call's exit: the trailer's, or what says nothing ran this time.
    pub fn status(&self) -> i32 {
        match self {
            Admission::Ended(trailer) => trailer.exit,
            Admission::Running { .. } => crate::Exit::Busy.as_status(),
            Admission::Lost { .. } | Admission::Never => crate::Exit::Failed.as_status(),
        }
    }
}

/// What a caller needs to follow or stop a job its call let go of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckInReport {
    pub job: JobId,
    /// The runner's, which `--kill` takes.
    pub pid: u32,
    /// How long the job runs between check-ins.
    #[serde(rename = "every")]
    pub every_secs: u64,
}

/// A `--port` name and the port the machine picked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickedPort {
    pub name: String,
    pub port: u16,
}

/// How a job ended.
///
/// Its JSON is stored: in a job's `trailer.json`, inside a [`Record`], and in dibspatcher's
/// store.
///
/// # Guarantees
///
/// - Reads the JSON every released runner wrote.
/// - [`Trailer::to_line`] writes the shape a machine has always printed:
///   `job <id>  <mode>  <label>  queued <n>s  ran <n>s  exit <n>  by=<source>`, then
///   `  built=<crates>` or `  built=nothing` once cargo finished, then `  tests=<n>` once a test
///   binary ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trailer {
    pub job: JobId,
    pub mode: Mode,
    pub label: Label,
    #[serde(rename = "queued")]
    pub queued_secs: u64,
    #[serde(rename = "ran")]
    pub ran_secs: u64,
    pub exit: i32,
    pub by: ExitSource,
    /// What cargo compiled, when the log shows cargo finishing.
    pub built: Option<Compilation>,
    /// The tests cargo's test runs ran together, when it ran any test binary: 0 is an exit 0 a
    /// green row would otherwise hide.
    #[serde(rename = "tests", default, skip_serializing_if = "Option::is_none")]
    pub test_count: Option<u64>,
}

impl Trailer {
    /// The line a job ends with, without its newline.
    pub fn to_line(&self) -> String {
        let built = self.built.map(Compilation::to_field);
        let tests = self.test_count.map(|tests| format!("  tests={tests}"));
        format!(
            "job {}  {}  {}  queued {}s  ran {}s  exit {}  by={}{}{}",
            self.job,
            self.mode,
            self.label,
            self.queued_secs,
            self.ran_secs,
            self.exit,
            self.by,
            built.unwrap_or_default(),
            tests.unwrap_or_default()
        )
    }
}

/// How many crates a job's cargo compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compilation {
    Crates(u64),
    Nothing,
}

impl Compilation {
    /// The trailer's `built=` field, with the two spaces before it.
    fn to_field(self) -> String {
        match self {
            Compilation::Crates(count) => format!("  built={count}"),
            Compilation::Nothing => "  built=nothing".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trailer() -> Trailer {
        Trailer {
            job: JobId::new("20261001-120000-4242"),
            mode: Mode::Shared,
            label: Label::new("app_build"),
            queued_secs: 0,
            ran_secs: 3,
            exit: 0,
            by: ExitSource::Command,
            built: None,
            test_count: None,
        }
    }

    #[test]
    fn a_trailer_reads_as_a_machine_has_always_printed_it() {
        assert_eq!(
            trailer().to_line(),
            "job 20261001-120000-4242  shared  app_build  queued 0s  ran 3s  exit 0  by=command"
        );
    }

    #[test]
    fn a_trailer_says_what_cargo_built() {
        let built = |built| {
            Trailer {
                built: Some(built),
                ..trailer()
            }
            .to_line()
        };
        assert!(built(Compilation::Nothing).ends_with("by=command  built=nothing"));
        assert!(built(Compilation::Crates(12)).ends_with("by=command  built=12"));
    }

    #[test]
    fn a_trailer_says_how_many_tests_ran_after_what_was_built() {
        let tests = |count| {
            Trailer {
                built: Some(Compilation::Crates(12)),
                test_count: Some(count),
                ..trailer()
            }
            .to_line()
        };
        assert!(tests(0).ends_with("built=12  tests=0"));
        assert!(tests(25).ends_with("built=12  tests=25"));
    }

    #[test]
    fn an_admission_ended_exits_as_its_trailer_did() {
        let ended = Admission::Ended(Trailer {
            exit: 3,
            ..trailer()
        });
        assert_eq!(ended.status(), 3);
    }

    #[test]
    fn an_admission_still_running_exits_busy() {
        let running = Admission::Running {
            job: JobId::new("20261001-120000-4242"),
            pid: 7,
        };
        assert_eq!(running.status(), crate::Exit::Busy.as_status());
    }

    #[test]
    fn an_admission_lost_or_never_made_exits_failed() {
        let lost = Admission::Lost {
            job: JobId::new("20261001-120000-4242"),
            started: true,
        };
        assert_eq!(lost.status(), crate::Exit::Failed.as_status());
        assert_eq!(Admission::Never.status(), crate::Exit::Failed.as_status());
    }
}
