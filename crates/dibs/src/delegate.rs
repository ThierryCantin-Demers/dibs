//! The bash client, which still answers every mode not yet ported.

use std::{os::unix::process::CommandExt as _, process::Command};

pub struct BashClient;

impl BashClient {
    const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../bin/dibs");

    /// The bash client with these words, reaching back to this binary for the recipe layer.
    pub fn command(words: &[String]) -> Command {
        let mut command = Command::new(BashClient::PATH);
        command.args(words);
        if let Ok(me) = std::env::current_exe() {
            command.env("DIBS_CORE", me);
        }
        command
    }

    /// Replaces this process with the bash client; returns only if that failed.
    pub fn exec(words: &[String]) -> std::io::Error {
        BashClient::command(words).exec()
    }
}
