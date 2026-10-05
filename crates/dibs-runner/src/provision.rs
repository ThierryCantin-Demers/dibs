use crate::{channel::Caller, session::Session, settings::home, sink::Sink, stop::Signals};
use dibs_format::{
    Label, Mode,
    wire::{MaxFrom, Request, Watch},
};
use std::{
    fs,
    io::{self, Read as _},
    path::Path,
};

/// How long a build of the runner may hold the shared lock.
pub const BUILD_MAX: u64 = 1800;
const HASH_DIGITS: usize = 16;
/// What the far shell exits with for a missing runner, so the client builds its own over this one.
const NOT_THIS_SOURCE: i32 = 125;

/// The runner source this was built from, by the hash that names it: empty for a runner built by
/// hand, which then serves no call.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub hash: &'a str,
}

impl Source<'_> {
    pub fn serves(&self, asked: &str) -> bool {
        !self.hash.is_empty() && self.hash == asked
    }

    /// Refuses before reading a byte of the request, which a client of other source wrote.
    pub fn refuse(&self, asked: &str) -> i32 {
        eprintln!(
            "dibs-runner: this is the runner of {}, not of {asked}, so it serves no call for {asked}.",
            self.described()
        );
        NOT_THIS_SOURCE
    }

    /// `dibs-runner hash`, which a build checks before it installs what cargo made.
    pub fn name(&self) -> i32 {
        match self.hash.is_empty() {
            true => {
                eprintln!("dibs-runner: {}", self.described());
                1
            }
            false => {
                println!("{}", self.hash);
                0
            }
        }
    }

    fn described(&self) -> &str {
        match self.hash.is_empty() {
            true => "no source, since install.sh did not build it",
            false => self.hash,
        }
    }
}

/// `dibs-runner build <hash>`: the tree on stdin, built under the shared lock as an ordinary job
/// and installed where a call for that hash looks. The one interface every runner keeps, so the
/// newest runner on a machine can build any later one.
pub fn build(hash: &str) -> i32 {
    let signals = Signals::block();
    let sink = Sink::plain();
    if hash.len() != HASH_DIGITS || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        sink.say(&format!("dibs-runner: {hash:?} is not a runner's hash\n"));
        return 2;
    }
    let mut tree = Vec::new();
    if io::stdin().read_to_end(&mut tree).is_err() || tree.is_empty() {
        sink.say("dibs-runner: no tree arrived on stdin to build\n");
        return 2;
    }
    let runners = home().join(".cache/dibs/runner");
    let pid = std::process::id();
    let archive = runners.join(format!(".tree.{hash}.{pid}.tar.gz"));
    let source = runners.join(format!(".src.{hash}.{pid}"));
    if let Err(e) = fs::create_dir_all(&source).and_then(|()| fs::write(&archive, tree)) {
        sink.say(&format!(
            "dibs-runner: the tree could not be kept in {}: {e}\n",
            runners.display()
        ));
        return 70;
    }
    let command = format!(
        "[ \"$({installed} hash 2>/dev/null)\" = {hash} ] && {{ echo 'dibs-runner {hash} is installed already.'; exit 0; }}\n\
         cd {source} && tar -xmzf {archive} && CARGO_TARGET_DIR={target} sh install.sh {hash}",
        installed = quoted(&runners.join(hash).join("dibs-runner")),
        source = quoted(&source),
        archive = quoted(&archive),
        target = quoted(&runners.join(".target")),
    );
    let request = Request {
        mode: Mode::Shared,
        label: Label::new("dibs-runner"),
        command,
        wait: None,
        max: BUILD_MAX,
        max_from: MaxFrom::Given,
        verbose: false,
        json: false,
        stream: false,
        tty: false,
        card: None,
        fingerprint: None,
        agent: "dibs-runner build".into(),
        agent_id: String::new(),
        batch: None,
        watch: Watch {
            off: true,
            hold: false,
            lease: 0,
        },
        ports: Vec::new(),
        services: Vec::new(),
        ready_within: 0,
        new_series: false,
    };
    let code = Session::new(request, sink)
        .with_temporary(vec![source.clone(), archive.clone()])
        .serve(Caller::Stdout, signals);
    let _ = fs::remove_dir_all(&source);
    let _ = fs::remove_file(&archive);
    code
}

/// A path as one word for the shell.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runner_serves_calls_for_its_own_source_alone() {
        let built = Source {
            hash: "0123456789abcdef",
        };
        assert!(built.serves("0123456789abcdef"));
        assert!(!built.serves("fedcba9876543210"));
        assert!(
            !Source { hash: "" }.serves(""),
            "nor does one built by hand"
        );
    }
}
