//! A recipe run: its refs and arms, its pins, the schedule of its jobs, and the record it leaves.

mod base;
mod jobs;
mod pins;
mod record;
mod refs;
mod schedule;
mod sweep;
#[cfg(test)]
mod tests;
mod with;

pub(crate) use base::{RunError, raw, run_recipe};
pub(crate) use schedule::recipe_jobs;
pub(crate) use with::with_service;
