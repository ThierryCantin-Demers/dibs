//! A job: its environment, the process and its cap, the kill tree, its services and ports, and
//! how it ended.

mod base;
mod environment;
mod held;
mod outcome;
mod ports;
mod reap;
mod services;

pub use base::{Cap, Job, Output};
pub use environment::{Environment, Unpinned};
pub use held::Held;
pub use outcome::{Digest, Repeat, built, job_id};
pub use ports::{PortRange, Ports};
pub use reap::{reap, tree_below};
pub use services::{Guard, Readiness, Services, Start};
