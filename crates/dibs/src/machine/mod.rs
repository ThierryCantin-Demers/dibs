//! One call to one machine: where it goes, what it sends, and how it ends.

mod card;
mod deadline;
mod held;
mod interrupt;
mod lines;
mod provision;
mod served;
mod session;
mod ssh;
mod target;
mod unreachable;
mod values;

pub use card::{Card, CardError};
pub use deadline::{Deadline, Stop, Stoppable};
pub use held::{Held, Holder, Release};
pub use interrupt::{Interrupt, Relayed};
pub use lines::{Lines, Listener, Stream};
pub use provision::{Installed, Provision, Queueing};
pub use served::{Client, Delivery, MISSING, RUNNER_WORD, Runner};
pub use session::{
    Answer, Diagnosis, Here, Kept, Liveness, Message, Reach, Route, SSH_FAILED, Session,
};
pub use ssh::Ssh;
pub use target::{Fleet, Named, Target, TargetEnv, TargetError};
pub use unreachable::Unreachable;
pub use values::{CallValues, MaxFrom, Watch};
