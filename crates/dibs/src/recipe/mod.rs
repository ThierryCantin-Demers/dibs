//! Recipes: what a repo and this computer declare, their parameters, labels and fingerprints,
//! and what is refused before anything is built.

mod base;
mod labels;
mod listing;
mod manifest;
mod refusals;
mod repo;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub use base::Isolation;
pub use base::{Recipe, Step};
pub use dibs_format::Lock;
#[cfg(test)]
pub use labels::label_steps;
pub use labels::run_label;
pub use listing::list;
pub use manifest::{Manifest, Service, local_dir};
#[cfg(test)]
pub use manifest::{Source, Verb};
pub use refusals::{RecipeError, Resolved, resolve};
pub use repo::{resolve_repo, root_of};
