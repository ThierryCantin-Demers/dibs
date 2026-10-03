use crate::{session::Session, settings::home, sink::Sink, stop::Signals};
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
const BUILD_MAX: u64 = 1800;
const HASH_DIGITS: usize = 16;

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
        "[ -x {installed} ] && {{ echo 'dibs-runner {hash} is installed already.'; exit 0; }}\n\
         cd {source} && tar -xzf {archive} && CARGO_TARGET_DIR={target} sh install.sh {hash}",
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
    let code = Session::new(request, sink).serve(None, signals);
    let _ = fs::remove_dir_all(&source);
    let _ = fs::remove_file(&archive);
    code
}

/// A path as one word for the shell.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}
