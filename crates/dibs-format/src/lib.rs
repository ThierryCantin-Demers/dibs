//! What dibs writes and reads, as data: no I/O happens here past reading the clock.
//!
//! Each tab-separated record writes the shape the machines write today, byte for byte, and
//! reads every shape a released dibs has written, so a client and a machine on different
//! versions agree; a [`LogLine`] is only written.
//!
//! Values are kept in their own types: the ids [`Label`], [`JobId`], [`BatchId`],
//! [`MachineName`] and [`Alias`], and [`FileName`], text as a machine files it; time as a
//! [`Moment`] and a duration as a [`Span`]; and bytes as [`Hex`], as [`Base64`], or digested by
//! [`Sha256`]. A stream's frames are read back out by a [`wire::FrameBuffer`].
//!
//! # Examples
//!
//! ```
//! use dibs_format::{Moment, Span};
//!
//! let started = Moment::in_zone(1_789_745_748, 0);
//! let ran = Span::from_secs(185);
//! assert_eq!(format!("{} for {ran}", started.minute()), "2026-09-18 15:35 for 3m05s");
//! ```

mod base64;
pub mod dibspatcher;
mod exit;
pub mod fleet;
mod friction;
mod hex;
mod ids;
mod lines;
pub mod lockfile;
mod mode;
mod moment;
mod run;
mod sha256;
mod span;
pub mod status;
pub mod wire;

pub use base64::Base64;
pub use exit::Exit;
pub use friction::FrictionNote;
pub use hex::Hex;
pub use ids::{Alias, BatchId, FileName, JobId, Label, MachineName};
pub use lines::{
    BatchPlan, Event, ExitSource, HistoryLine, JobMeta, LineError, LockRecord, LogLine,
    PendingStep, PendingStepKind,
};
pub use mode::{Lock, Mode};
pub use moment::Moment;
pub use run::{ArmRecord, Outcome, Pair, Pairs, ProcedureStep, RunRecord, RunVerb, StepRecord};
pub use sha256::Sha256;
pub use span::Span;
