//! What `dibs status` says of one machine, as one document: its JSON is the document, and its
//! text is rendered from it.

mod base;
mod metrics;
mod text;

pub use base::{
    Ahead, BatchShown, Closure, Holder, Idle, IdleKind, Left, Leftover, Listing, LockState, Orphan,
    Overrun, Remaining, Scene, Scope, Service, Shown, Silent, Status, Waiter, Within,
};
pub use metrics::{GpuMetrics, Metrics, Vendor};
pub use text::Text;
