//! A job: its environment, the process and its cap, the kill tree, its services and ports, and
//! how it ended.

mod base;
mod environment;
mod held;
mod outcome;
mod ports;
mod reap;
mod services;
mod tether;
mod tree;

pub use base::{Cap, Job, Output};
pub use environment::{Environment, Unpinned};
pub use held::Held;
pub use outcome::{Digest, LogRead, Repeat, job_id};
pub use ports::{PortRange, Ports};
pub use reap::{reap, tree_below};
pub use services::{Guard, Readiness, Services, Start};
pub use tether::Tether;
pub use tree::Tree;
