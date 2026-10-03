//! A job: its environment, the process and its cap, the kill tree, and how it ended.

mod base;
mod environment;
mod outcome;
mod reap;

pub use base::{Cap, Job, Output};
pub use environment::{Environment, Unpinned};
pub use outcome::{Digest, Repeat, built, job_id};
pub use reap::reap;
