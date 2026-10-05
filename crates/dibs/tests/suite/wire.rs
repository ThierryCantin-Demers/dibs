//! What a call sends to a machine, captured by an ssh that writes down its arguments and the
//! request frame ahead of the job's stream.

use crate::harness::*;
use crate::recipes::{PARAMS, app, recipes};
use crate::snapshot::*;
use std::fs;

/// `WIRE_RUN=1` runs the far side here, a host apiece under `WIRE_HOSTS`, else exits `WIRE_EXIT`;
/// `WIRE_STDIN=1` keeps all its stdin after the request too.
/// `ssh -G` reaches no machine, so it answers as ssh would and is not recorded.
const RECORDING_SSH: &str = r#"#!/bin/bash
[ "$1" = -G ] && { echo "hostname ${2##*@}"; exit 0; }
n=1; while ! mkdir "$WIRE/$n.slot" 2>/dev/null; do n=$((n + 1)); done
printf '%s\0' ssh "$@" > "$WIRE/$n.argv"
cmd=${@: -1} host=${@: -2:1}
if [[ $cmd == *'"$r" serve'* ]]; then
    IFS= read -r header
    head -c "${header#request }" > "$WIRE/$n.request"
    { printf '%s\n' "$header"; cat "$WIRE/$n.request"; } > "$WIRE/$n.frame"
fi
[ -n "${WIRE_SAYS:-}" ] && printf '%s\n' "$WIRE_SAYS" >&2
if [ "${WIRE_RUN:-0}" = 1 ]; then
    [ -n "${WIRE_FAR_CARGO_HOME:-}" ] && export CARGO_HOME=$WIRE_FAR_CARGO_HOME
    if [ -n "${WIRE_HOSTS:-}" ]; then
        far=$WIRE_HOSTS/${host#*@}
        export DIBS_LOCK_DIR=$far/lock DIBS_HISTORY=$far/history DIBS_LOG=$far/log DIBS_SCRATCH=$far/scratch
        mkdir -p "$DIBS_LOCK_DIR" "$DIBS_SCRATCH"
    fi
    if [ -e "$WIRE/$n.frame" ] && [ "${WIRE_STDIN:-0}" = 1 ]; then
        exec bash -c "$cmd" < <(cat "$WIRE/$n.frame"; exec tee "$WIRE/$n.rest")
    fi
    if [ -e "$WIRE/$n.frame" ]; then exec bash -c "$cmd" < <(cat "$WIRE/$n.frame"; exec cat); fi
    exec bash -c "$cmd"
fi
exit "${WIRE_EXIT:-0}"
"#;

/// An rsync that records what it copies and reaches the far side through its -e program.
const RECORDING_RSYNC: &str = r#"#!/bin/bash
[ "$1" = --version ] && { echo 'rsync  version 3.2.7  protocol version 31'; exit 0; }
rsh=ssh args=() remote=
while [ $# -gt 0 ]; do
    case $1 in
        -e) rsh=$2; shift 2; continue ;;
        --rsh=*) rsh=${1#--rsh=}; shift; continue ;;
        *:*) remote=$1 ;;
    esac
    args+=("$1"); shift
done
n=1; while ! mkdir "$WIRE/$n.slot" 2>/dev/null; do n=$((n + 1)); done
printf '%s\0' rsync "${args[@]}" > "$WIRE/$n.argv"
host=${remote%%:*}
case $host in *@*) set -- -l "${host%@*}" "${host#*@}" ;; *) set -- "$host" ;; esac
exec $rsh "$@" rsync --server -logDtprze.iLsfxCIvu . "${remote#*:}"
"#;

/// A terminal for a call to write to, so it says what it would say to a person.
const PTY: &str =
    "import os, pty, sys; sys.exit(os.waitstatus_to_exitcode(pty.spawn(sys.argv[1:])))";

const MACHINES: &str = "[machine.box-a]\nssh      = \"dibs@box-a\"\nhostname = \"box-a\"\n\n  [[machine.box-a.device]]\n  kind     = \"gpu\"\n  alias    = \"gpu:card\"\n  name     = \"a card\"\n  pci      = \"0000:01:00.0\"\n  chip     = \"10de:2786\"\n  runtimes = [\"cuda\", \"vulkan\"]\n\n[machine.box-b]\nssh      = \"dibs@box-b\"\nhostname = \"box-b\"\n";

/// A sandbox whose calls leave this computer for box-a over the recording ssh.
pub(crate) fn wired() -> Sandbox {
    let mut s = Sandbox::new();
    s.write_exec("wirebin/ssh", RECORDING_SSH);
    fs::create_dir_all(s.path("wire")).unwrap();
    s.set("WIRE", s.p("wire"));
    s.set("PATH", format!("{}:{}", s.p("wirebin"), s.var("PATH")));
    s.set("DIBS_LOCAL", "0");
    s.machines(MACHINES);
    s
}

fn wire_normal(s: &Sandbox) -> Normal {
    Normal::of(s)
        .rule(r"\b[0-9]+-[0-9]{16,}\b", "<token>")
        .rule(env!("DIBS_RUNNER_HASH"), "<hash>")
        .rule(r"local-[0-9a-f]{10}\b", "local-<key>")
        .rule(r#""key":"[0-9a-f]{10}""#, r#""key":"<key>""#)
        .rule(
            r"(local:[0-9a-f]{7,12})(\+dirty)?-[0-9a-f]{12}\b",
            "local:<sha>$2-<content>",
        )
        .rule(r"\b[0-9a-f]{40}\b", "<commit>")
        .rule(r"\b[0-9a-f]{12}\b", "<sha>")
        .rule(r"\b[0-9a-f]{7}\b", "<sha>")
        .rule(r"pin-[0-9a-f]{10}\b", "pin-<hash>")
        .rule(
            r"rsync --server[^'\n]*",
            "rsync --server <what this rsync asks of its far side>",
        )
}

/// The words a shell makes of a command, NUL-separated, as the machine's `bash -c` runs it.
const WORDS: &str = r#"eval "set -- $1"; printf '%s\0' "$@""#;

/// How a call's command is compared.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Cmd {
    /// As one shell string, the way it was given.
    AsSent,
    /// By the words the machine's shell makes of it, however the arguments were quoted.
    AsWords,
}

/// Whether a capture's calls come one after another or all at once, in no particular order.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Calls {
    InOrder,
    AtOnce,
}

/// One snapshot's worth of captures from a wired sandbox.
pub(crate) struct Wire<'a> {
    s: &'a Sandbox,
    n: Normal,
    t: Transcript,
}

impl<'a> Wire<'a> {
    pub(crate) fn new(s: &'a Sandbox) -> Wire<'a> {
        Wire {
            s,
            n: wire_normal(s),
            t: Transcript::default(),
        }
    }

    /// One call's wire, under the call.
    pub(crate) fn record(&mut self, title: &str, call: Call) -> Output {
        self.record_as(title, call, Cmd::AsSent, Calls::InOrder)
    }

    pub(crate) fn record_as(&mut self, title: &str, call: Call, cmd: Cmd, calls: Calls) -> Output {
        self.clear();
        let out = call.run();
        let body = format!("-> exit {}\n{}", out.code, self.captured(cmd, calls));
        self.t.section(&self.n.apply(title), &body);
        out
    }

    pub(crate) fn text(&self) -> &str {
        self.t.text()
    }

    fn clear(&self) {
        let wire = self.s.path("wire");
        let _ = fs::remove_dir_all(&wire);
        fs::create_dir_all(&wire).unwrap();
    }

    /// What the recorded calls sent, with the machine script checked rather than shown.
    fn captured(&self, cmd: Cmd, calls: Calls) -> String {
        let wire = self.s.path("wire");
        let mut numbers: Vec<usize> = fs::read_dir(&wire)
            .unwrap()
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_string_lossy()
                    .strip_suffix(".argv")
                    .and_then(|k| k.parse().ok())
            })
            .collect();
        numbers.sort();
        let mut blocks: Vec<String> = numbers.iter().map(|&k| self.call(k, cmd)).collect();
        if calls == Calls::AtOnce {
            blocks.sort();
        }
        blocks
            .iter()
            .enumerate()
            .map(|(i, b)| format!("call {}:{b}", i + 1))
            .collect()
    }

    /// One recorded call, from just after its number.
    fn call(&self, k: usize, cmd: Cmd) -> String {
        let wire = self.s.path("wire");
        let argv = fs::read(wire.join(format!("{k}.argv"))).unwrap();
        let argv: Vec<String> = argv
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let multiline = argv.last().is_some_and(|a| a.contains('\n'));
        let (head, command) = match multiline {
            true => (&argv[..argv.len() - 1], argv.last()),
            false => (&argv[..], None),
        };
        let head: Vec<&str> = head.iter().map(String::as_str).collect();
        let mut out = format!(" {}\n", self.n.apply(&typed(&head)));
        if let Some(c) = command {
            out.push_str("  its command, for the login shell there:\n");
            out.push_str(&indent(&self.n.apply(c)));
        }
        if let Ok(request) = fs::read_to_string(wire.join(format!("{k}.request"))) {
            out.push_str("  the request the runner acts on:\n");
            out.push_str(&indent(&self.n.apply(&self.request(&request, cmd))));
        }
        out
    }

    /// A request's fields, one a line, a card's and the watch's under their names.
    fn request(&self, request: &str, cmd: Cmd) -> String {
        let value: serde_json::Value = serde_json::from_str(request)
            .unwrap_or_else(|e| panic!("the request is not JSON: {e}\n{request}"));
        let mut entries = Vec::new();
        for (name, field) in value.as_object().expect("a request is an object") {
            match field {
                serde_json::Value::Object(inner) => {
                    for (key, value) in inner {
                        entries.push(format!("{name}.{key}: {value}\n"));
                    }
                }
                serde_json::Value::String(command) if name == "command" => {
                    entries.push(self.entry("command", command, cmd));
                }
                serde_json::Value::String(text) if !text.contains('\n') => {
                    entries.push(format!("{name}: {text}\n"));
                }
                serde_json::Value::String(text) => {
                    entries.push(self.entry(name, text, Cmd::AsSent))
                }
                other => entries.push(format!("{name}: {other}\n")),
            }
        }
        entries.sort();
        entries.concat()
    }

    /// One value as the machine sees it; a script's comments and blank lines are left out, since
    /// they change nothing it does.
    fn entry(&self, name: &str, value: &str, cmd: Cmd) -> String {
        if let Some(array) = name.strip_suffix("[]") {
            return format!("{array}: an array of {value}\n");
        }
        if (name == "CMD" || name == "command") && cmd == Cmd::AsWords {
            return format!("{name}, as the words it runs:\n{}", self.words(value));
        }
        if !value.contains('\n') {
            return format!("{name}: {value}\n");
        }
        let lines: String = value
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
            .map(|l| format!("  | {l}\n"))
            .collect();
        format!("{name}:\n{lines}")
    }

    fn words(&self, command: &str) -> String {
        let out = self.s.command("bash", ["-c", WORDS, "_", command]).run();
        assert_eq!(out.code, 0, "{command} is not words: {}", out.stderr);
        out.stdout
            .split('\0')
            .filter(|w| !w.is_empty())
            .map(|w| format!("  | {w}\n"))
            .collect()
    }
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("    {l}\n")).collect()
}

#[test]
fn what_each_call_sends() {
    let mut s = wired();
    s.write_exec("fakersync/rsync", RECORDING_RSYNC);
    s.set("PATH", format!("{}:{}", s.p("fakersync"), s.var("PATH")));
    let mut w = Wire::new(&s);
    let calls: Vec<&[&str]> = vec![
        &["--on", "box-a", "--label", "one-string", "echo hi; exit 3"],
        &["--on", "box-a", "--bench", "--label", "measured", "true"],
        &[
            "--on", "box-a", "--bench", "--device", "gpu:card", "--label", "pinned", "true",
        ],
        &[
            "--on", "box-a", "--wait", "5", "--max", "60", "--stream", "-v", "--label", "flags",
            "true",
        ],
        &[
            "--on",
            "box-a",
            "--label",
            "served",
            "--port",
            "api",
            "--with",
            "srv=./serve --port $DIBS_PORT_API",
            "--ready",
            "tcp:api",
            "--ready-within",
            "9",
            "--with",
            "side=true",
            "true",
        ],
        &["--on", "box-a", "--hold", "--label", "held", "true"],
        &["--on", "box-a", "--peek", "ls -la"],
        &["--on", "box-a", "--status"],
        &["--on", "box-a", "--status", "--json"],
        &["--on", "box-a", "-v", "--status"],
        &["--on", "box-a", "--watch", "3"],
        &["--on", "box-a", "--watch", "--json"],
        &["--on", "box-a", "--log", "7"],
        &["--on", "box-a", "--kill", "4242"],
        &["--on", "box-a", "--kill", "4242", "--anyone", "--force"],
        &["--on", "box-a", "--out"],
        &["--on", "box-a", "--out", "4242"],
        &["--on", "box-a", "--out", "20260101120000-4242"],
        &["--on", "box-a", "--fetch", "20260101120000-4242"],
        &["--on", "box-a", "--fetch", "20260101120000-4242", "./got"],
        &["--on", "box-a", "--release"],
        &["--on", "box-a", "--gc"],
        &["--on", "box-a", "--gc", "--days", "3", "--dry-run"],
        &["--on", "box-a", "--check"],
        &["--check", "box-a", "--write"],
        &["--on", "box-a", "--sync", "-a", "./x", ":~/y"],
        &[
            "--on",
            "box-a",
            "--label",
            "sent",
            "--sync",
            "--checksum",
            ":~/y/",
            "./x/",
        ],
    ];
    for args in calls {
        w.record(&format!("dibs {}", typed(args)), s.dibs(args));
    }
    let several = [
        "--on", "box-a", "--label", "several", "printf", "%s|", "it's", "a b", "$HOME",
    ];
    w.record_as(
        &format!("dibs {}", typed(&several)),
        s.dibs(several),
        Cmd::AsWords,
        Calls::InOrder,
    );
    w.record(
        "dibs --on box-a --status  (on a terminal)",
        s.command("python3", ["-c", PTY, DIBS, "--on", "box-a", "--status"]),
    );
    s.write(
        "steps",
        "[build] dibs --on box-a --label step true\n[measure] dibs --on box-a --bench --label gemm true\n",
    );
    w.record(
        "dibs batch steps  (two steps on box-a)",
        s.dibs(["batch", &s.p("steps")]),
    );
    w.record_as(
        "dibs --kill 20260101-120000-42  (a batch driven from elsewhere)",
        s.dibs(["--kill", "20260101-120000-42"]),
        Cmd::AsSent,
        Calls::AtOnce,
    );
    s.write(
        "fleet.toml",
        "[machine.box-a]\nprovisioned = { by = \"hand\" }\n\n[machine.box-b]\nprovisioned = { by = \"hand\" }\n",
    );
    w.record_as(
        "dibs machines",
        s.dibs(["machines"]).env("DIBS_FLEET", s.p("fleet.toml")),
        Cmd::AsSent,
        Calls::AtOnce,
    );
    let steps: [(&str, Call); 6] = [
        (
            "DIBS_AGENT=\"it's \\\"mine\\\"\"",
            s.dibs(["--on", "box-a", "--label", "step", "true"])
                .env("DIBS_AGENT", "it's \"mine\""),
        ),
        (
            "with no session",
            s.dibs(["--on", "box-a", "--label", "step", "true"])
                .no_session(),
        ),
        (
            "DIBS_LEASE=0",
            s.dibs(["--on", "box-a", "--label", "step", "true"])
                .env("DIBS_LEASE", "0"),
        ),
        (
            "DIBS_NO_LIVE=1",
            s.dibs(["--on", "box-a", "--label", "step", "true"])
                .env("DIBS_NO_LIVE", "1"),
        ),
        (
            "DIBS_NO_WATCHDOG=1",
            s.dibs(["--on", "box-a", "--label", "step", "true"])
                .env("DIBS_NO_WATCHDOG", "1"),
        ),
        (
            "DIBS_TRACE=1 DIBS_CONNECT_TIMEOUT=3",
            s.dibs(["--on", "box-a", "--label", "step", "true"])
                .env("DIBS_TRACE", "1")
                .env("DIBS_CONNECT_TIMEOUT", "3"),
        ),
    ];
    for (env, call) in steps {
        w.record(&format!("{env} dibs --on box-a --label step true"), call);
    }
    w.record(
        "dibs --on box-a --status  (refused, so asked again why)",
        s.dibs(["--on", "box-a", "--status"])
            .env("WIRE_EXIT", "255")
            .env("WIRE_SAYS", "dibs@box-a: Permission denied (publickey)."),
    );
    w.record(
        "dibs --check box-c --write  (a machine the inventory does not have yet)",
        s.dibs(["--check", "box-c", "--write"]),
    );
    snapshot("wire", w.text());
}

#[test]
fn what_a_recipe_sends() {
    let s = wired();
    let dir = app(&s);
    s.write_exec(
        "home/.cargo/bin/cargo",
        "#!/bin/bash\necho \"    Finished \\`release\\` profile [optimized] target(s) in 0.01s\"\n",
    );
    recipes(
        &s,
        &format!(
            "{PARAMS}\n[bench.gate]\n  [[bench.gate.step]]\n  lock = \"shared\"\n  run = \"cargo build --release\"\n  [[bench.gate.step]]\n  lock = \"exclusive\"\n  run = \"echo measured\"\n\n[bench.art]\nartifacts = [\"results/*.json\"]\n  [[bench.art.step]]\n  lock = \"exclusive\"\n  run = \"mkdir -p results && echo measured > results/new.json\"\n\n[service.servers]\nbuild = \"true\"\nports = [\"api\"]\n\n[[service.servers.serve]]\nname = \"api\"\nrun = \"read -r _ < \\\"$WIRE_NEVER\\\"\"\n"
        ),
    );
    s.write("app/Cargo.lock", "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n");
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "lock"]);
    s.git("app", &["push", "-q", "origin", "HEAD:main"]);
    s.git(".", &["init", "-q", "lib"]);
    s.write(
        "lib/Cargo.toml",
        "[package]\nname = \"serde\"\nversion = \"1.0.0\"\n",
    );
    s.write("lib/src/lib.rs", "");
    s.git("lib", &["add", "-A"]);
    s.git("lib", &["commit", "-qm", "lib"]);
    let never = s.gate("never");
    let mut w = Wire::new(&s);
    let run = |args: &[&str]| {
        s.dibs(args)
            .env("WIRE_RUN", "1")
            .env("WIRE_NEVER", never.path.display().to_string())
    };
    let (main, local) = (format!("{dir}@main"), format!("{dir}@local"));
    let lib = format!("{}@local", s.p("lib"));
    let got = s.p("got");
    let features: [&[&str]; 7] = [
        &["build", &main, "p", "--on", "box-a", "--device", "gpu:card"],
        &[
            "build",
            &main,
            "p",
            "--on",
            "box-a",
            "--sweep",
            "samples=1,2",
        ],
        &[
            "bench",
            &format!("{dir}@origin/main..local"),
            "gate",
            "--on",
            "box-a",
            "--reps",
            "1",
        ],
        &["bench", &local, "art", "--on", "box-a", "--artifacts", &got],
        &["with", &local, "servers", "--on", "box-a", "--", "true"],
        &[
            "with", &local, "servers", "--there", "--on", "box-a", "--", "true",
        ],
        &["build", &local, "p", "--on", "box-a", "--pin", &lib],
    ];
    for args in [
        &[
            "build",
            &format!("{dir}@main"),
            "p",
            "--on",
            "box-a",
            "--samples",
            "3",
        ][..],
        &["bench", &format!("{dir}@local"), "gate", "--on", "box-a"],
        &[
            "shell",
            &format!("{dir}@local"),
            "--reason",
            "a look",
            "--on",
            "box-a",
            "--",
            "cat a.txt",
        ],
    ] {
        let out = w.record(&format!("dibs {}", typed(args)), run(args));
        assert_eq!(out.code, 0, "{args:?}: {}", out.all());
    }
    for args in features {
        w.record(&format!("dibs {}", typed(args)), run(args));
    }
    let commit = s.git("lib", &["rev-parse", "HEAD"]);
    s.git(
        ".",
        &[
            "clone",
            "-q",
            "--bare",
            "lib",
            "home/.cargo/git/db/dep-0123456789abcdef",
        ],
    );
    s.write(
        "app/Cargo.lock",
        &format!("{}\n[[package]]\nname = \"dep\"\nversion = \"1.0.0\"\nsource = \"git+https://example.invalid/dep.git#{commit}\"\n", s.read("app/Cargo.lock")),
    );
    s.git("app", &["commit", "-qam", "a git dependency"]);
    s.git("app", &["push", "-q", "origin", "HEAD:main"]);
    w.record(
        "dibs build app@local p --on box-a  (a git dependency the machine lacks)",
        run(&["build", &local, "p", "--on", "box-a"]).env("WIRE_FAR_CARGO_HOME", s.p("far-cargo")),
    );
    snapshot("wire-recipes", w.text());
}
