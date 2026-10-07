//! What this computer keeps of the work it sent: run records, friction notes, and which machine
//! holds each repo's build cache.

mod affinity;
mod error;
mod friction;
mod runs;

pub use affinity::Affinity;
pub use error::{Kept, RecordsError};
pub use friction::{Complaints, FrictionLog};
pub use runs::{RunLog, Runs, date, now_secs};
