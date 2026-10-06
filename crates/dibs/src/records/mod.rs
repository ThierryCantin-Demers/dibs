//! What this computer keeps of the work it sent: run records, friction notes, and which machine
//! holds each repo's build cache.

mod affinity;
mod error;
pub mod friction;
pub mod runs;

pub use affinity::{affinity_get, affinity_set, pinned};
pub use error::{Kept, RecordsError};
pub use runs::{now_secs, runs_path, write_record};
