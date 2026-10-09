//! Shell completion: what fits where the cursor is, answered from this computer's own files so a
//! Tab never waits on a machine. The shells' scripts only ask `dibs __complete`.

mod flags;
mod shell;
mod sources;
mod wanted;

#[cfg(test)]
mod tests;

pub use shell::Shell;
pub use sources::Sources;
pub use wanted::Wanted;
