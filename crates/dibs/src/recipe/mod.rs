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
pub(crate) use base::Isolation;
pub(crate) use base::{Recipe, Step};
pub(crate) use dibs_format::Lock;
#[cfg(test)]
pub(crate) use labels::label_steps;
pub(crate) use labels::run_label;
pub(crate) use listing::list;
pub(crate) use manifest::{Manifest, Service, local_dir};
#[cfg(test)]
pub(crate) use manifest::{Source, Verb};
pub(crate) use refusals::{RecipeError, Resolved, resolve};
pub(crate) use repo::{resolve_repo, root_of};
