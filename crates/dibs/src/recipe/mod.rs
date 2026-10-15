//! Recipes: what a repo and this computer declare, their parameters, labels and fingerprints,
//! and what is refused before anything is built.

mod base;
mod error;
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
pub use error::{Flaw, ManifestError, NotTaken, ParamError, RecipeError, RepoError, ShellWords};
pub use labels::run_label;
pub use listing::list;
#[cfg(test)]
pub use manifest::Source;
pub use manifest::Verb;
pub use manifest::{Manifest, Service};
pub use refusals::Resolved;
pub use repo::Checkouts;
