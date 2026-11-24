//! What `dibs status` says of one machine, as one document: its JSON is the document, and its
//! text is rendered from it.

mod base;
mod metrics;
mod text;

pub use base::{
    Ahead, BatchShown, Closure, Holder, Idle, IdleKind, IdleWait, Left, Leftover, Listing,
    LockState, Orphan, Overrun, Remaining, Scene, Scope, Service, Shown, Silent, Status, Stopped,
    Waiter, Within,
};
pub use metrics::{Disk, DiskRole, GpuMetrics, Metrics, Quota, QuotaKind, Vendor};
pub use text::Text;
