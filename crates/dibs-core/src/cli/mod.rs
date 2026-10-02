//! The `dibs` command line: one grammar for every form, read into a typed `Invocation`.
//!
//! The grammar is irregular and frozen: agents type it from memory and from the rules file.

mod base;
mod error;
mod help;
mod parse;
mod recipe;
mod shell;

#[cfg(test)]
mod tests;

pub use base::{
    Call, Friction, Invocation, KillTarget, Mode, OutTarget, PortName, Run, RunLock, Service,
    ServiceName,
};
pub use error::CliError;
pub use help::Help;
pub use recipe::{RecipeCall, RecipeVerb, Sweep};
pub use shell::{BashQuoted, Command, ShellWord};
