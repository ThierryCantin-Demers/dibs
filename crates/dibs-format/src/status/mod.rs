//! What `dibs status --json` says of one machine, as one document. The runner renders its text
//! from the same values, with what only the text shows beside them.

mod base;
mod metrics;

pub use base::{
    BatchShown, BatchTimeLeft, Closure, Holder, HolderState, IdleKind, IdleWait, Idleness, JobCall,
    LockState, LowPriorityStop, Overrun, PreemptionState, RecipePlace, RemainingBasis, Scope,
    SeriesHere, ShownEstimate, Silence, Status, Waiter,
};
pub use metrics::{Disk, DiskRole, GpuMetrics, Metrics, Quota, QuotaKind, Vendor};
