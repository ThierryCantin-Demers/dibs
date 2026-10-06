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

pub use base::{RunError, raw, run_recipe};
pub use build::{build_signature, packages};
pub use local::{
    Checkout, Local, as_fetched, checkout, identity, local, toplevel, unfetchable, variant,
};
pub use refs::{commit, merge_base};
pub use schedule::recipe_jobs;
pub use trees::Nest;
pub use with::with_service;
