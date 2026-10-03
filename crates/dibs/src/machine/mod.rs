//! One call to one machine: where it goes, what it sends, and how it ends.

mod half;
mod held;
mod interrupt;
mod lines;
mod payload;
mod provision;
mod served;
mod session;
mod ssh;
mod target;
mod unreachable;

pub use half::Half;
pub use held::{Held, Holder, Release};
pub use interrupt::Interrupt;
pub use lines::{Lines, Stream};
pub use payload::{CallValues, Card, MaxFrom, Watch, decode, encode};
pub use provision::{Installed, Provision};
pub use served::{Delivery, RUNNER_WORD, Runner};
pub use session::{
    Answer, Diagnosis, Here, Kept, Liveness, Message, Reach, Route, Session, Started, exit_code,
};
pub use ssh::Ssh;
pub use target::{Fleet, Named, Target, TargetEnv, TargetError, after_at};
pub use unreachable::{Unreachable, no_room};
