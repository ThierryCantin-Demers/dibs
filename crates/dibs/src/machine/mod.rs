//! One call to one machine: where it goes, what it sends, and how it ends.

mod half;
mod lines;
mod payload;
mod served;
mod session;
mod target;
mod unreachable;

pub use half::Half;
pub use lines::{Lines, Stream};
pub use payload::{CallValues, Card, MaxFrom, Watch, decode, encode};
pub use served::{RUNNER_WORD, Runner};
pub use session::{
    Answer, Diagnosis, Here, Interrupt, Kept, Liveness, Message, Reach, Route, Session, Ssh,
    Started, exit_code,
};
pub use target::{Fleet, Named, Target, TargetEnv, TargetError, after_at};
pub use unreachable::{Unreachable, no_room};
