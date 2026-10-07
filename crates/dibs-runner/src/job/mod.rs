//! A job: its environment, the process and its cap, the kill tree, its services and ports, and
//! how it ended.

mod base;
mod environment;
mod held;
mod outcome;
mod ports;
mod services;
mod tether;
mod tree;

pub use base::{Cap, Job, JobEnd, Output};
pub use environment::{Environment, Unpinned};
pub use held::HoldFifo;
pub use outcome::{Digest, LogRead, Repeat};
pub use ports::{PortRange, Ports};

pub use services::{Guard, Readiness, Services, Start};
pub use tether::Tether;
pub use tree::Tree;
