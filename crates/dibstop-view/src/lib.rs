//! What dibstop shows, whatever draws it: one row per job with what is worth saying about it,
//! the one-off `dibs` calls a person can start from one, and, behind the `egui` feature, a
//! view a window or a web page draws. The caller feeds it each machine's status and runs the
//! actions it asks for, so the view holds no process or connection of its own.

pub mod action;
pub mod item;
pub mod text;
#[cfg(feature = "egui")]
pub mod view;
