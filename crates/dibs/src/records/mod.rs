//! What this computer keeps of the work it sent: run records, friction notes, and which machine
//! holds each repo's build cache.

mod affinity;
mod error;
mod friction;
mod measurement;
mod runs;

pub use affinity::Affinity;
pub use error::{Ledger, RecordsError};
pub use friction::{Complaints, FrictionLog};
pub use measurement::{Measurement, Unmeasurable, measurement_of, with_values};
pub use runs::{RunLog, Runs};
