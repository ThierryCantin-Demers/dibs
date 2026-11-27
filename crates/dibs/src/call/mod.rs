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

pub use base::{
    ATTEMPT, ATTEMPT_LOOKUP, BUNDLE_SPEC, CHECK_IN, CallError, DEFAULT_CHECK_IN, IMAGE,
    LOW_PRIORITY, LockedCall, Ran, TRAILERS, image_digest,
};
pub use dispatch::Dispatch;
pub use feed::{Fed, StatusFeed};
pub use hold::Guard;
pub use kept::reached_its_job;
pub use kill::{Driver, DriverClaim, Sender};
pub use local::Destination;
pub use machine::{Asked, Bound, MachineCall};
pub use origin::{BatchStep, Origin, Pending, RecipeJob};
pub use output::Output;
pub use series::Series;
pub use sync::{Rsh, Sync};
pub use watched::{Starter, Watched};
