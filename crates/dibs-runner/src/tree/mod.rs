//! Trees on a machine: a worktree per commit or sent checkout, a build cache per repo, seeded
//! from a sibling's, and the sweep that collects what nobody uses.

mod base;
mod builds;
mod copy;
mod git;
mod packages;
mod seed;
mod sweep;
#[cfg(test)]
mod tests;

pub use base::Trees;
pub use copy::{Copier, Reflinks};
pub use git::Commands;
