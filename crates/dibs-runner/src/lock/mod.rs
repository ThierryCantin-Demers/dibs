//! The gate and `rw` flocks, and the records that say who holds the lock and who waits.

mod base;
mod records;

pub use base::{Hold, Lock};
pub use records::{Kind, LockDir, pid_of, still_the_same};
