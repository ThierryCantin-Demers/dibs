//! One call: a run, a benchmark, a peek or a hold, a mode the machine half answers, or one
//! answered here from the inventory.

mod base;
mod check;
mod dispatch;
mod feed;
mod hold;
mod kept;
mod kill;
mod local;
mod machine;
mod origin;
mod output;
mod series;
mod status;
mod sync;
mod watched;

pub use base::{CallError, LockedCall};
pub use dispatch::Dispatch;
pub use feed::{Fed, StatusFeed};
pub use hold::Guard;
pub use kill::{Driver, DriverClaim};
pub use local::Destination;
pub use machine::{Asked, Bound, MachineCall};
pub use origin::{BatchStep, Origin, Pending, Planned, RecipeJob};
pub use output::Output;
pub use sync::{Rsh, Sync};
pub use watched::{Starter, Watched};
