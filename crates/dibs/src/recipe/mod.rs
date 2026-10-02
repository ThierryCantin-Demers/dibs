//! Recipes: what a repo and this computer declare, their parameters, labels and fingerprints,
//! and what is refused before anything is built.

mod fingerprint;
mod labels;
mod listing;
mod manifest;
mod params;
mod refusals;
mod repo;
#[cfg(test)]
mod tests;

pub(crate) use dibs_format::Lock;
#[cfg(test)]
pub(crate) use labels::label_steps;
pub(crate) use labels::run_label;
pub(crate) use listing::list;
#[cfg(test)]
pub(crate) use manifest::{Isolation, Source, Verb};
pub(crate) use manifest::{Manifest, Recipe, Service, Step, local_dir};
pub(crate) use refusals::{RecipeError, Resolved, resolve};
pub(crate) use repo::{resolve_repo, root_of};
