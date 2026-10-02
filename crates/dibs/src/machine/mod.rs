//! One call to one machine: where it goes, what it sends, and how it ends.

mod lines;
mod payload;
mod session;
mod target;
mod unreachable;

pub use lines::{Lines, Stream};
pub use payload::{CallValues, Card, MachineHalf, MaxFrom, Watch, decode, encode};
pub use session::{
    Answer, Diagnosis, Here, Interrupt, Kept, Liveness, Message, Reach, Route, Session, Ssh,
    Started, exit_code,
};
pub use target::{Fleet, Named, Target, TargetEnv, TargetError, after_at};
pub use unreachable::{Unreachable, no_room};
