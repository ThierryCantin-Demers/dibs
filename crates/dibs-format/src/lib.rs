//! What dibs writes and reads, as data: no I/O happens here past reading the clock.
//!
//! Each line codec reads every shape a released dibs has written and writes the shape the
//! machines write today, byte for byte, so a client and a machine on different versions agree.

pub mod base64;
mod exit;
pub mod fleet;
mod friction;
mod ids;
mod lines;
pub mod lockfile;
mod mode;
mod moment;
mod run;
mod span;
pub mod status;
pub mod wire;

pub use exit::Exit;
pub use friction::FrictionNote;
pub use ids::{Alias, BatchId, FileName, JobId, Label, MachineName};
pub use lines::{
    BatchPlan, By, Event, HistoryLine, JobMeta, LineError, LockRecord, LogLine, PendingKind,
    PendingStep,
};
pub use mode::{Lock, Mode};
pub use moment::Moment;
pub use run::{ArmRecord, Outcome, Pair, Pairs, ProcedureStep, RunRecord, RunVerb, StepRecord};
pub use span::Span;
