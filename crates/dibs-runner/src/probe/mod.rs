//! What `dibs --check` reports of a machine: what dibs needs there, and what is in it.

mod base;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub use base::Probe;
#[cfg(target_os = "linux")]
use linux::Gpus;
#[cfg(target_os = "macos")]
use macos::Gpus;
