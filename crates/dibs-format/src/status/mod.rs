//! What `dibs status` says of one machine, as one document: its JSON is the document, and its
//! text is rendered from it.

mod base;
mod text;

pub use base::{
    BatchShown, Holder, Idle, IdleKind, Left, Listing, LockState, Orphan, Remaining, Scene, Scope,
    Service, Shown, Status, Waiter,
};
pub use text::Text;
