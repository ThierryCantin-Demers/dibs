//! What the client decides and says on its own, which a client that replaces the bash one has to
//! decide and say the same way: where a call is placed, a status across machines, a recipe run's
//! account of itself, `with`, and a hold's servers.

use crate::harness::*;
use crate::recipes::{PARAMS, app, recipes};
use crate::snapshot::*;
use crate::snapshots::INVENTORY;
use crate::wire::wired;
use std::fs;

/// box-a and box-b, each with a lock directory and state of its own, their far sides run here.
fn fleet() -> Sandbox {
    let mut s = wired();
    s.set("WIRE_RUN", "1");
    s.set("WIRE_HOSTS", s.p("hosts"));
    s
}

fn holders_on(s: &Sandbox, machine: &str) -> usize {
    fs::read_dir(s.path(&format!("hosts/{machine}/lock")))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("holder."))
        .count()
}

fn client_normal(s: &Sandbox) -> Normal {
    Normal::of(s)
        .clocked()
        .pids()
        .rule(r"\b20[5-9][0-9]{2}\b", "<port>")
        .rule(r"\b[0-9]+-[0-9]{16,}\b", "<token>")
        .rule(r"local-[0-9a-f]{10}\b", "local-<key>")
        .rule(r"\b[0-9a-f]{40}\b", "<commit>")
        .rule(r"\b[0-9a-f]{12}\b", "<sha>")
        .rule(r"\b[0-9a-f]{10}\b", "<sha>")
        .rule(r"\b[0-9a-f]{7}\b", "<sha>")
        .rule(r"\b[0-9]+(\.[0-9]+)?(ms|s)\b", "<dur>")
}

#[test]
fn placement_and_status_across_machines() {
    let mut s = fleet();
    // Both far sides share this computer's load, so only what the inventory says can tell them apart.
    s.machines("[machine.box-a]\nssh      = \"dibs@box-a\"\nhostname = \"box-a\"\n\n[machine.box-b]\nssh      = \"dibs@box-b\"\nhostname = \"box-b\"\nmeasure  = false\n");
    let n = client_normal(&s).rule(r"\b[0-9]+% busy", "<n>% busy");
    let mut t = Transcript::default();
    t.section(
        "dibs --pick -v  (both idle, box-b measures nothing)",
        &n.output(&s.dibs(["--pick", "-v"]).run()),
    );
    let busy = s.gate("busy");
    let job = s.spawn(s.dibs(["--on", "box-a", "--bench", "--label", "busy", &busy.hold()]));
    s.until_records("box-a to hold a benchmark", || holders_on(&s, "box-a") == 1);
    t.section(
        "dibs status  (box-a measuring)",
        &n.output(&s.dibs(["status"]).run()),
    );
    t.section(
        "dibs --pick -v  (box-a measuring)",
        &n.output(&s.dibs(["--pick", "-v"]).run()),
    );
    t.section(
        "dibs --label placed true  (no --on, box-a measuring)",
        &n.output(&s.dibs(["--label", "placed", "true"]).run()),
    );
    busy.open();
    s.wait(job);
    snapshot("fleet", t.text());
}

#[test]
fn a_recipe_run_says_what_it_does() {
    let mut s = Sandbox::new();
    // A cp that copies where it is asked to share blocks, so a new tree is seeded on any disk.
    let cp = s.command("bash", ["-c", "type -P cp"]).run().stdout;
    s.write_exec(
        "cow/cp",
        &format!(
            "#!/bin/bash\nargs=()\nfor a; do [ \"$a\" = --reflink=always ] || args+=(\"$a\"); done\nexec {} \"${{args[@]}}\"\n",
            cp.trim()
        ),
    );
    s.set("PATH", format!("{}:{}", s.p("cow"), s.var("PATH")));
    s.machines(INVENTORY);
    let dir = app(&s);
    s.write_exec(
        "home/.cargo/bin/cargo",
        "#!/bin/bash\necho \"   Compiling app v0.1.0\"\necho \"    Finished \\`release\\` profile [optimized] target(s) in 0.01s\"\n",
    );
    recipes(
        &s,
        &format!(
            "{PARAMS}\n[bench.gate]\n  [[bench.gate.step]]\n  lock = \"shared\"\n  run = \"cargo build --release\"\n  [[bench.gate.step]]\n  lock = \"exclusive\"\n  run = \"echo measured\"\n"
        ),
    );
    s.write(
        "app/Cargo.lock",
        "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "recipes"]);
    s.git("app", &["push", "-q", "origin", "HEAD:main"]);
    s.git("app", &["worktree", "add", "-q", &s.p("app-topk")]);
    let n = client_normal(&s);
    let mut t = Transcript::default();
    let local = format!("{dir}@local");
    let topk = format!("{}@local", s.p("app-topk"));
    let ab = format!("{dir}@origin/main..local");
    let calls: [&[&str]; 6] = [
        &["build", &local, "p"],
        &["bench", &topk, "gate"],
        &["bench", &ab, "gate", "--reps", "2"],
        &["build", &local, "p", "--sweep", "samples=1,2"],
        &["bench", &local, "gate", "--reps", "2"],
        &[
            "bench",
            &local,
            "gate",
            "--on",
            "box-a",
            "--device",
            "gpu:card",
            "--dry-run",
        ],
    ];
    for args in calls {
        t.section(
            &n.apply(&format!("dibs {}", typed(args))),
            &n.output(&s.dibs(args).run()),
        );
    }
    snapshot("recipe-narrative", t.text());
}

#[test]
fn with_and_a_hold_name_their_servers() {
    let s = Sandbox::new();
    let dir = app(&s);
    let never = s.gate("never");
    recipes(
        &s,
        &format!(
            "[service.servers]\nbuild = \"true\"\nports = [\"api\"]\n\n[[service.servers.serve]]\nname = \"api\"\nrun = \"read -r _ < {}\"\n",
            never.path.display()
        ),
    );
    let n = client_normal(&s);
    let mut t = Transcript::default();
    let local = format!("{dir}@local");
    let says = "echo port=$DIBS_PORT_API service=$DIBS_SERVICE_API";
    for args in [
        &["with", &local, "servers", "--", says][..],
        &["with", &local, "servers", "--there", "--", says],
        &[
            "--hold",
            "--port",
            "api",
            "--with",
            &format!("srv={}", never.hold()),
            "--label",
            "held",
            says,
        ],
    ] {
        t.section(
            &n.apply(&format!("dibs {}", typed(args))),
            &n.output(&s.dibs(args).run()),
        );
    }
    let s = fleet();
    let never = s.gate("never");
    let n = client_normal(&s);
    let args = [
        "--on",
        "box-a",
        "--hold",
        "--port",
        "api",
        "--with",
        &format!("srv={}", never.hold()),
        "--label",
        "held",
        says,
    ];
    t.section(
        &n.apply(&format!("dibs {}", typed(&args))),
        &n.output(&s.dibs(args).run()),
    );
    snapshot("with", t.text());
}
