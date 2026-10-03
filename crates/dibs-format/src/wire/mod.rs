//! What a client and its runner say to each other: `docs/design/protocol.md`.

mod frame;
mod record;
mod request;

pub use frame::{Frame, FrameError, Unframer};
pub use record::{Built, Picked, Record, Trailer};
pub use request::{Card, MaxFrom, Request, Service, Watch};
