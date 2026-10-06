//! Trees on a machine: a worktree per commit or sent checkout, a build cache per repo, seeded
//! from a sibling's, and the sweep that collects what nobody uses.

mod base;
mod builds;
mod clocks;
mod copy;
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

pub use base::Trees;
pub use copy::{Copier, Mark, Reflinks};
pub use gc::{Asked, human};
pub use git::Commands;
pub use step::Stepping;
