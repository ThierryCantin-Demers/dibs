//! The gate and `rw` flocks, and the records that say who holds the lock and who waits.

mod base;
mod cards;
pub mod cohort;
mod records;

pub use base::{Hold, Lock};
pub use cards::Cards;
pub use records::{Entry, GUARD_WAIT, Kind, LockDir, RecordFile, Urgent};
