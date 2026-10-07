//! What differs between operating systems, behind one trait.

mod base;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub use base::{Descendants, Platform, Slot};
#[cfg(target_os = "linux")]
pub use linux::Linux as Host;
#[cfg(target_os = "macos")]
pub use macos::MacOs as Host;
