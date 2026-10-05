//! A recipe run: its refs and arms, its pins, the schedule of its jobs, and the record it leaves.

mod base;
mod build;
mod jobs;
mod local;
mod pins;
mod record;
mod refs;
mod schedule;
mod sweep;
#[cfg(test)]
mod tests;
mod trees;
mod with;

pub(crate) use base::{RunError, raw, run_recipe};
pub(crate) use build::{build_signature, packages};
pub(crate) use local::{
    Checkout, Local, as_fetched, checkout, identity, local, toplevel, unfetchable, variant,
};
pub(crate) use refs::{commit, merge_base};
pub(crate) use schedule::recipe_jobs;
pub(crate) use trees::{Nest, SYNC_ARGS};
pub(crate) use with::with_service;
