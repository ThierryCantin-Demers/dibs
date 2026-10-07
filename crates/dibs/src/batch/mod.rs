//! One submission, one wake: a list of dibs command lines run by a driver on this side, with one
//! summary at the end. An agent is woken once for the list rather than once per job, and each
//! wake re-reads its whole context, so this is where most of a busy day's cost goes.
//!
//! The driver is the owner and adds no state on any machine. Each step is the dibs call the
//! agent would have made; if the driver dies its steps die with it and their locks release,
//! which is the lifetime a single job already has. The design is `dibs-design/batch.md`.

mod base;
mod guard;
mod parse;
mod plan;
#[cfg(test)]
mod tests;

pub use base::{Options, batch_id, run};
pub use guard::StepGuard;
pub use parse::{BadLine, BatchError};
