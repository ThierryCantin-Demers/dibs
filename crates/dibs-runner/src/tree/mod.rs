//! Trees on a machine: a worktree per commit or sent checkout, a build cache per repo, seeded
//! from a sibling's, and the sweep that collects what nobody uses.

mod base;
mod builds;
mod clocks;
mod copy;
#[cfg(test)]
mod edges;
mod error;
mod gc;
mod git;
mod glob;
mod packages;
mod runners;
mod seed;
mod step;
mod sweep;
#[cfg(test)]
mod tests;

pub use base::{TreeConfig, Trees};
pub use clocks::Clocks;
pub use copy::{Mark, Reflinks};
pub use gc::{Asked, Bytes};
pub use git::Commands;
pub use runners::Runners;
pub use step::{BuildMark, Spot, Stepping};
