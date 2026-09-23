//! dibs end to end, on this computer only: every test in a sandbox of its own, run in parallel.
//! What needs a real ssh channel is in tests/dibs-live-test.sh.

mod harness;

mod batch;
mod cli;
mod detach;
mod hold;
mod identity;
mod lock;
mod machines;
mod output;
mod recipes;
mod scratch;
mod services;
mod status;
mod sync;
mod transport;
mod update;
