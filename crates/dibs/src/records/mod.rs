//! What this computer keeps of the work it sent: run records, friction notes, and which machine
//! holds each repo's build cache.

mod affinity;
pub(crate) mod friction;
pub(crate) mod runs;

pub(crate) use affinity::{affinity_get, affinity_set, pinned};
pub(crate) use runs::{now_secs, runs_path, write_record};
