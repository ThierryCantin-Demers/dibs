//! What a call sends to a machine, captured by an ssh that writes down its arguments and decodes
//! the script ahead of the job's stream. A client that replaces the bash one has to send the same
//! header and the same values, and the machine script after them unchanged.

use crate::harness::*;
use crate::recipes::{PARAMS, app, recipes};
use crate::snapshot::*;
use std::fs;

/// Records each call in `$WIRE/<n>.argv`, its arguments NUL-separated, and the script it sent,
/// decoded, in `$WIRE/<n>.payload`. With `WIRE_RUN=1` it then runs the far side here, on the
/// same stream, as the machine would; otherwise it exits `WIRE_EXIT`, saying `WIRE_SAYS` first.
const RECORDING_SSH: &str = r#"#!/bin/bash
n=$(( $(find "$WIRE" -name '*.argv' | wc -l) + 1 ))
printf '%s\0' ssh "$@" > "$WIRE/$n.argv"
cmd=${@: -1}
count=$(printf '%s\n' "$cmd" | sed -n 's/.*count=\([0-9][0-9]*\).*/\1/p' | head -n 1)
if [ -n "$count" ]; then
    head -c "$count" > "$WIRE/$n.b64"
    base64 -d < "$WIRE/$n.b64" | gzip -dc > "$WIRE/$n.payload"
fi
[ -n "${WIRE_SAYS:-}" ] && printf '%s\n' "$WIRE_SAYS" >&2
if [ "${WIRE_RUN:-0}" = 1 ]; then
    if [ -n "$count" ]; then exec bash -c "$cmd" < <(cat "$WIRE/$n.b64"; exec cat); fi
    exec bash -c "$cmd"
fi
exit "${WIRE_EXIT:-0}"
"#;

/// An rsync that writes down how dibs started it and which machine it was handed.
const RECORDING_RSYNC: &str = r#"#!/bin/bash
[ "$1" = --help ] && { echo '  --mkpath   create destination path components'; exit 0; }
n=$(( $(find "$WIRE" -name '*.argv' | wc -l) + 1 ))
printf '%s\0' rsync "$@" > "$WIRE/$n.argv"
for v in DIBS_HOST DIBS_HOSTNAME DIBS_SYNC_LABEL DIBS_ON; do printf '%s=%s\n' "$v" "${!v-<unset>}"; done > "$WIRE/$n.env"
[ -n "${DIBS_RSH_EXIT:-}" ] && echo "DIBS_RSH_EXIT=<a file>" >> "$WIRE/$n.env"
exit 0
"#;

const MACHINES: &str = "[machine.box-a]\nssh      = \"dibs@box-a\"\nhostname = \"box-a\"\n\n  [[machine.box-a.device]]\n  kind     = \"gpu\"\n  alias    = \"gpu:card\"\n  name     = \"a card\"\n  pci      = \"0000:01:00.0\"\n  chip     = \"10de:2786\"\n  runtimes = [\"cuda\", \"vulkan\"]\n\n[machine.box-b]\nssh      = \"dibs@box-b\"\nhostname = \"box-b\"\n";

/// A sandbox whose calls leave this computer for box-a over the recording ssh.
fn wired() -> Sandbox {
    let mut s = Sandbox::new();
    s.write_exec("wirebin/ssh", RECORDING_SSH);
    fs::create_dir_all(s.path("wire")).unwrap();
    s.set("WIRE", s.p("wire"));
    s.set("PATH", format!("{}:{}", s.p("wirebin"), s.var("PATH")));
    s.set("DIBS_LOCAL", "0");
    s.machines(MACHINES);
    s
}

/// lib/machine, as a call sends it after its values.
fn machine_script() -> String {
    let mut parts: Vec<_> = fs::read_dir(repo_root().join("lib/machine"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    parts.sort();
    parts
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect()
}

fn wire_normal(s: &Sandbox) -> Normal {
    Normal::of(s)
        .rule(r"count=[0-9]+", "count=<n>")
        .rule(
            r"\.dibs-payload\.[0-9]+\.[0-9]+\.sh",
            ".dibs-payload.<pid>.<time>.sh",
        )
        .rule(r"\b[0-9]+-[0-9]{16,}\b", "<token>")
        .rule(r"local-[0-9a-f]{10}\b", "local-<key>")
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

/// Reads a payload's values back the way the machine does, by running them, and prints each
/// as `NAME`, NUL, value, NUL in the order sent; an array's members as `NAME[i]`.
const READ_VALUES: &str = r#"set -u
. "$1"
for name in $(sed -n 's/^\([A-Z_][A-Z_]*\)=.*/\1/p; s/^declare -a \([A-Z_][A-Z_]*\)=.*/\1/p' "$1"); do
    if declare -p "$name" 2>/dev/null | grep -q '^declare -a'; then
        declare -n members=$name
        printf '%s\0%s\0' "$name[]" "${#members[@]}"
        for i in "${!members[@]}"; do printf '%s\0%s\0' "$name[$i]" "${members[$i]}"; done
        unset -n members
    else
        printf '%s\0%s\0' "$name" "${!name}"
    fi
done
"#;

/// The values a payload carries, as the machine sees them once it has run them: one per line, a
/// value of several lines indented under its name.
fn values(s: &Sandbox, sent: &str) -> String {
    s.write("values.sh", sent);
    let out = s
        .command("bash", ["-c", READ_VALUES, "_", &s.p("values.sh")])
        .run();
    assert_eq!(out.code, 0, "the values do not run: {}\n{sent}", out.stderr);
    let fields: Vec<&str> = out.stdout.split('\0').collect();
    let mut text = String::new();
    for pair in fields.chunks(2) {
        let [name, value] = pair else { continue };
        if let Some(count) = name.strip_suffix("[]") {
            text.push_str(&format!("{count}: an array of {value}\n"));
        } else if value.contains('\n') {
            text.push_str(&format!("{name}:\n"));
            text.extend(value.split('\n').map(|l| format!("  | {l}\n")));
        } else {
            text.push_str(&format!("{name}: {value}\n"));
        }
    }
    text
}

/// What the recorded calls sent, in order, with the machine script checked rather than shown.
fn captured(s: &Sandbox, n: &Normal) -> String {
    let wire = s.path("wire");
    let mut calls: Vec<usize> = fs::read_dir(&wire)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_suffix(".argv")
                .and_then(|k| k.parse().ok())
        })
        .collect();
    calls.sort();
    let script = machine_script();
    let mut out = String::new();
    for k in calls {
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
        out.push_str(&format!("call {k}: {}\n", n.apply(&typed(&head))));
        if let Some(c) = command {
            out.push_str("  its command, for the login shell there:\n");
            out.push_str(&indent(&n.apply(c)));
        }
        if let Ok(env) = fs::read_to_string(wire.join(format!("{k}.env"))) {
            out.push_str("  with:\n");
            out.push_str(&indent(&n.apply(&env)));
        }
        if let Ok(payload) = fs::read_to_string(wire.join(format!("{k}.payload"))) {
            let sent = payload.strip_suffix(script.as_str()).unwrap_or_else(|| {
                panic!(
                    "call {k} sent something other than lib/machine after its values:\n{payload}"
                )
            });
            out.push_str("  its values, ahead of lib/machine:\n");
            out.push_str(&indent(&n.apply(&values(s, sent))));
        }
    }
    out
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("    {l}\n")).collect()
}

fn clear(s: &Sandbox) {
    for e in fs::read_dir(s.path("wire")).unwrap().flatten() {
        let _ = fs::remove_file(e.path());
    }
}

/// One call's wire, under the call.
fn record(s: &Sandbox, n: &Normal, t: &mut Transcript, title: &str, call: Call) {
    clear(s);
    let out = call.run();
    t.section(title, &format!("-> exit {}\n{}", out.code, captured(s, n)));
}

#[test]
fn what_each_call_sends() {
    let mut s = wired();
    s.write_exec("fakersync/rsync", RECORDING_RSYNC);
    s.set("PATH", format!("{}:{}", s.p("fakersync"), s.var("PATH")));
    let n = wire_normal(&s);
    let mut t = Transcript::default();
    let calls: Vec<&[&str]> = vec![
        &["--on", "box-a", "--label", "one-string", "echo hi; exit 3"],
        &[
            "--on", "box-a", "--label", "several", "printf", "%s|", "it's", "a b", "$HOME",
        ],
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
        &["--on", "box-a", "--release"],
        &["--on", "box-a", "--gc"],
        &["--on", "box-a", "--gc", "--days", "3", "--dry-run"],
        &["--on", "box-a", "--check"],
        &["--check", "box-a", "--write"],
        &["--on", "box-a", "--abi"],
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
        record(
            &s,
            &n,
            &mut t,
            &format!("dibs {}", typed(args)),
            s.dibs(args),
        );
    }
    let rsh = |label: &str| {
        s.dibs([
            "--rsh",
            "box-a",
            "rsync",
            "--server",
            "-logDtprze.iLsfxCIvu",
            ".",
            "~/y",
        ])
        .env("DIBS_HOST", "dibs@box-a")
        .env("DIBS_HOSTNAME", "box-a")
        .env("DIBS_ON", "box-a")
        .env("DIBS_SYNC_LABEL", label)
    };
    record(
        &s,
        &n,
        &mut t,
        "dibs --rsh box-a rsync --server ...  (as rsync starts it, under the sync's label)",
        rsh("sent"),
    );
    s.write("before.sh", "mkdir -p ~/y\ncd ~/y\n");
    record(
        &s,
        &n,
        &mut t,
        "dibs --rsh box-a rsync --server ...  (with DIBS_SYNC_BEFORE naming a script)",
        rsh("sync").env("DIBS_SYNC_BEFORE", s.p("before.sh")),
    );
    let steps: [(&str, Call); 9] = [
        (
            "DIBS_BATCH=<batch> DIBS_BATCH_STEP=build DIBS_BATCH_PLAN=...",
            s.dibs(["--on", "box-a", "--label", "step", "true"])
                .env("DIBS_BATCH", "20260101-120000-42")
                .env("DIBS_BATCH_STEP", "build")
                .env("DIBS_BATCH_PLAN", "1\t2\nmeasure\tbench\tgemm\t1\n"),
        ),
        (
            "DIBS_FINGERPRINT='ab c/d!0123456789abcdef'",
            s.dibs(["--on", "box-a", "--label", "step", "true"])
                .env("DIBS_FINGERPRINT", "ab c/d!0123456789abcdef"),
        ),
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
        (
            "DIBS_REMOTE_DIR=/dev/shm",
            s.dibs(["--on", "box-a", "--label", "step", "true"])
                .env("DIBS_REMOTE_DIR", "/dev/shm"),
        ),
    ];
    for (env, call) in steps {
        record(
            &s,
            &n,
            &mut t,
            &format!("{env} dibs --on box-a --label step true"),
            call,
        );
    }
    let refused = s
        .dibs(["--on", "box-a", "--status"])
        .env("WIRE_EXIT", "255")
        .env("WIRE_SAYS", "dibs@box-a: Permission denied (publickey).");
    record(
        &s,
        &n,
        &mut t,
        "dibs --on box-a --status  (refused, so asked again why)",
        refused,
    );
    snapshot("wire", t.text());
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
            "{PARAMS}\n[bench.gate]\n  [[bench.gate.step]]\n  lock = \"shared\"\n  run = \"cargo build --release\"\n  [[bench.gate.step]]\n  lock = \"exclusive\"\n  run = \"echo measured\"\n"
        ),
    );
    s.write("app/Cargo.lock", "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n");
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "lock"]);
    s.git("app", &["push", "-q", "origin", "HEAD:main"]);
    let n = wire_normal(&s);
    let mut t = Transcript::default();
    let run = |args: &[&str]| s.dibs(args).env("WIRE_RUN", "1");
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
        clear(&s);
        let out = run(args).run();
        assert_eq!(out.code, 0, "{args:?}: {}", out.all());
        t.section(
            &format!("dibs {}", n.apply(&typed(args))),
            &format!("-> exit {}\n{}", out.code, captured(&s, &n)),
        );
    }
    snapshot("wire-recipes", t.text());
}
