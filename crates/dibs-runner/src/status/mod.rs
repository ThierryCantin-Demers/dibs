//! The status document, built from the lock directory's records and what the system says of the
//! processes they name, and its text.

mod base;
mod batch;
mod cpu;
mod scene;
mod text;

pub use base::Look;
pub use text::Snapshot;
