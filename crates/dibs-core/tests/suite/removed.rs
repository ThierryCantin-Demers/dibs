//! What the features being removed print and send, so that removing them is one diff here.

use crate::harness::*;
use crate::snapshot::*;
use crate::snapshots::{INVENTORY, Refusal, refusal_table};
use crate::wire::{Wire, wired};

#[test]
fn removed_features() {
    let s = wired();
    let mut w = Wire::new(&s);
    w.record("dibs --on box-a --abi", s.dibs(["--on", "box-a", "--abi"]));
    w.record(
        "DIBS_REMOTE_DIR=/dev/shm dibs --on box-a --label step true",
        s.dibs(["--on", "box-a", "--label", "step", "true"])
            .env("DIBS_REMOTE_DIR", "/dev/shm"),
    );

    let mut s = Sandbox::new();
    let n = Normal::of(&s);
    let mut t = Transcript::default();
    s.machines(INVENTORY);
    s.write("registry.toml", "[machine.team-box]\nssh      = \"dibs@team-box\"\nhostname = \"team-box\"\n\n[machine.box-b]\nssh      = \"elsewhere\"\nhostname = \"elsewhere\"\n");
    s.set("DIBS_REGISTRY_CACHE", s.p("registry.toml"));
    t.section(
        "dibs --machines  (with a shared registry under your own)",
        &n.output(&s.dibs(["--machines"]).run()),
    );
    for args in [
        &["--forget", "team-box"][..],
        &["--forget", "box-b"],
        &["--machines"],
    ] {
        t.section(
            &format!("dibs {}", typed(args)),
            &n.output(&s.dibs(args).run()),
        );
    }

    let s = Sandbox::new();
    let n = Normal::of(&s).rule(r"(bash: line) [0-9]+:", "$1 <n>:");
    let r = Refusal::of;
    let refusals = refusal_table(
        &s,
        &n,
        vec![
            r(&["--job"]),
            r(&["--cancel"]),
            r(&["list", "app"])
                .env("DIBS_CORE", "")
                .env("HOME", "/nonexistent"),
            r(&["--rsh"]),
            r(&["--rsh", "host", "rsync", "--server"]),
            r(&["--job", "12345"]).env("DIBS_QUEUE", ""),
            r(&["--cancel", "12345"]).env("DIBS_QUEUE", ""),
            r(&["--detach", "true"]).env("DIBS_QUEUE", ""),
            r(&["--bench", "--detach", "true"]).env("DIBS_QUEUE", "box@elsewhere"),
            r(&["--detach", "--wait", "5", "true"]).env("DIBS_QUEUE", "box@elsewhere"),
        ],
    );
    snapshot(
        "removed",
        &format!("{}\n{}\n== refused\n{refusals}", w.text(), t.text()),
    );
}
