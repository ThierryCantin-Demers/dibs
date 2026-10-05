//! What a client and its runner say to each other: `docs/design/protocol.md`.

mod frame;
mod record;
mod request;
mod tree;

pub use frame::{Frame, FrameError, Unframer};
pub use record::{Built, Picked, Record, Trailer};
pub use request::{Card, MaxFrom, Request, Service, Watch};
pub use tree::{
    At, GitDb, GitDbs, Nest, Packages, Place, Prepare, Prepared, Revision, Seeded, Shared, Source,
    Then, Tree,
};

/// The hash of the runner source a machine's `install.sh` built this from, None anywhere else.
/// Read in the crate the runner's others depend on, so a new hash rebuilds them all.
pub const SOURCE_HASH: Option<&str> = option_env!("DIBS_RUNNER_HASH");
