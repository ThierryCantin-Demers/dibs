//! What prepares leave beyond the plain cases `prepares.rs` pins: a tree seeded from a sibling
//! and reseeded when its lockfile moves, a pinned tree and its config, a recipe's `fresh` paths,
//! the sweep a prepare makes of old trees, targets and jobs, and a git database sent ahead of a
//! build. Each section also holds what the client said of the tree and recorded of it.

use crate::harness::*;
use crate::prepares::{LOCK, short_hash, tree};
use crate::recipes::{PARAMS, app, recipes};
use crate::snapshot::*;
use crate::wire::wired;
use std::fs;

/// The lockfile with `extra` more git packages, each a line of the tree's package list.
fn lock_with(extra: usize) -> String {
    let mut lock = LOCK.to_string();
    for i in 0..extra {
        lock += &format!(
            "\n[[package]]\nname = \"g{i}\"\nversion = \"0.1.0\"\nsource = \"git+https://example.invalid/g{i}?rev=r{i}#r{i}\"\n"
        );
    }
    lock
}

/// What the client said of the trees it prepared, and the tree facts of the run it recorded.
fn said(s: &Sandbox, n: &Normal, out: &Output) -> String {
    let mut text = format!("-> exit {}\n", out.code);
    for (mark, stream) in [("1|", &out.stdout), ("2|", &out.stderr)] {
        for line in stream.lines().filter(|l| l.starts_with("dibs: ")) {
            text += &format!("{mark} {}\n", n.apply(line));
        }
    }
    let runs = s.read("home/.local/state/dibs/runs.jsonl");
    if let Some(last) = runs.lines().last() {
        let run: serde_json::Value = serde_json::from_str(last).unwrap();
        for key in ["revisions", "seeded", "arms"] {
            if let Some(v) = run.get(key) {
                text += &format!("recorded {key}: {}\n", n.apply(&v.to_string()));
            }
        }
    }
    text
}

#[test]
fn the_scratch_seeds_reseeds_pins_and_fresh_paths_leave() {
    let mut s = Sandbox::new();
    // Reflinks made plain copies, so a new tree is seeded on any disk.
    s.set("DIBS_REFLINK", "copy");
    let dir = app(&s);
    s.write_exec(
        "home/.cargo/bin/cargo",
        "#!/bin/bash\necho \"   Compiling app v0.1.0\"\necho \"    Finished \\`release\\` profile [optimized] target(s) in 0.01s\"\n",
    );
    s.write("app/.gitignore", "target\ncache\n");
    s.write("app/Cargo.lock", LOCK);
    recipes(
        &s,
        &format!(
            "{PARAMS}\n[build.store]\n  [[build.store.step]]\n  lock = \"shared\"\n  run = \"mkdir -p cache && echo kept > cache/store\"\n\n[bench.gate]\n  [[bench.gate.step]]\n  lock = \"shared\"\n  run = \"cargo build --release\"\n  [[bench.gate.step]]\n  lock = \"exclusive\"\n  run = \"echo measured\"\n\n[tree]\nfresh = [\"cache\"]\n"
        ),
    );
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "recipes and a lockfile"]);
    s.git("app", &["push", "-q", "origin", "HEAD:main"]);
    let main = s.git("app", &["rev-parse", "HEAD"]);
    s.git("app", &["checkout", "-q", "-b", "decoy"]);
    s.write("app/a.txt", "decoy\n");
    s.git("app", &["commit", "-qam", "decoy"]);
    s.git("app", &["push", "-q", "origin", "decoy"]);
    let decoy = s.git("app", &["rev-parse", "HEAD"]);
    s.git("app", &["checkout", "-q", "-"]);
    s.git("app", &["worktree", "add", "-q", &s.p("app-topk")]);
    s.git(".", &["init", "-q", "lib"]);
    s.write(
        "lib/Cargo.toml",
        "[package]\nname = \"serde\"\nversion = \"1.0.0\"\n",
    );
    s.write("lib/src/lib.rs", "");
    s.git("lib", &["add", "-A"]);
    s.git("lib", &["commit", "-qm", "lib"]);

    let mut n = Normal::of(&s);
    for (path, name) in [
        (dir.clone(), "app"),
        (s.p("app-topk"), "app-topk"),
        (s.p("lib"), "lib"),
    ] {
        n = n.literal(
            &format!("local-{}", short_hash(&path)),
            &format!("local-<{name}>"),
        );
    }
    n = n
        .literal(&main[..12], "<main>")
        .literal(&decoy[..12], "<decoy>")
        .rule(r"\b[0-9]+-[0-9]{16,}\b", "<token>")
        .rule(r"pin-[0-9a-f]{10}\b", "pin-<hash>")
        .rule(
            r"(local:[0-9a-f]{7,12})(\+dirty)?-[0-9a-f]{12}\b",
            "local:<sha>$2-<content>",
        );
    let (local, topk) = (format!("{dir}@local"), format!("{}@local", s.p("app-topk")));
    let lib = format!("{}@local", s.p("lib"));
    let both = format!("{dir}@main,decoy");
    let mut t = Transcript::default();
    let step = |t: &mut Transcript, what: &str, args: &[&str]| {
        let out = s.dibs(args).run();
        assert_eq!(out.code, 0, "{what}: {}", out.all());
        t.section(
            &format!("{what}: dibs {}", n.apply(&typed(args))),
            &format!(
                "{}{}\n",
                said(&s, &n, &out),
                n.apply(&tree(&s.path("scratch"), &n).join("\n"))
            ),
        );
    };
    step(
        &mut t,
        "a working tree keeps a cache a fresh path names",
        &["build", &local, "store"],
    );
    step(&mut t, "and builds", &["bench", &local, "gate"]);
    step(
        &mut t,
        "a second checkout starts from the first, without the fresh path",
        &["bench", &topk, "gate"],
    );
    step(
        &mut t,
        "two fetched arms, the second seeded",
        &["bench", &both, "gate"],
    );
    s.write("app/Cargo.lock", &lock_with(12));
    step(
        &mut t,
        "the working tree moves to a new lockfile and builds it",
        &["bench", &local, "gate"],
    );
    s.write("app-topk/Cargo.lock", &lock_with(12));
    // A tree prepared minutes ago may be about to be entered, so only an older one is reseeded.
    let topk_target = format!("scratch/target/app-local-{}", short_hash(&s.p("app-topk")));
    s.command(
        "touch",
        [
            "-d",
            "1 hour ago",
            &s.p(&format!("{topk_target}/.dibs-used")),
        ],
    )
    .run();
    step(
        &mut t,
        "the second checkout, which has built far less of it, is reseeded",
        &["bench", &topk, "gate"],
    );
    step(
        &mut t,
        "a pinned tree, nested under its config",
        &["build", &local, "p", "--pin", &lib],
    );
    snapshot("trees", t.text());
}

#[test]
fn the_scratch_a_prepare_sweeps() {
    let s = Sandbox::new();
    let dir = app(&s);
    recipes(&s, PARAMS);
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "recipes"]);
    let scratch = s.path("scratch");
    let old = |rel: &str| {
        let p = scratch.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        if !p.exists() {
            fs::write(&p, "").unwrap();
        }
        s.command("touch", ["-d", "400 days ago", &p.display().to_string()])
            .run();
    };
    for d in [
        "ws/app/recent",
        "ws/other/unmarked",
        "target/unmarked/release",
        "target/recent",
        "jobs/recent",
    ] {
        fs::create_dir_all(scratch.join(d)).unwrap();
    }
    fs::write(scratch.join("ws/app/recent/.dibs-used"), "").unwrap();
    fs::write(scratch.join("target/recent/.dibs-used"), "").unwrap();
    old("ws/app/abandoned/.dibs-used");
    old("ws/other/abandoned/f");
    old("ws/other/abandoned/.dibs-used");
    old("target/abandoned/release/app");
    old("target/abandoned/.dibs-used");
    fs::create_dir_all(scratch.join("target/swept-only")).unwrap();
    fs::write(scratch.join("target/swept-only/.dibs-used"), "swept\n").unwrap();
    old("target/held/debug/.cargo-lock");
    old("target/held/.dibs-used");
    old("target/stuck/sub/f");
    old("target/stuck/.dibs-used");
    fs::create_dir_all(scratch.join("jobs/abandoned")).unwrap();
    s.command(
        "touch",
        [
            "-d",
            "400 days ago",
            &scratch.join("jobs/abandoned").display().to_string(),
        ],
    )
    .run();
    s.command(
        "chmod",
        [
            "500",
            &scratch.join("target/stuck/sub").display().to_string(),
        ],
    )
    .run();
    let n = Normal::of(&s)
        .rule(r"local-[0-9a-f]{10}\b", "local-<key>")
        .rule(
            r"(local:[0-9a-f]{7,12})(\+dirty)?-[0-9a-f]{12}\b",
            "local:<sha>$2-<content>",
        );
    let jobs = |when: &str| {
        let mut names: Vec<String> = fs::read_dir(scratch.join("jobs"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .collect();
        names.sort();
        format!(
            "jobs {when}, apart from those dibs ran: {}\n",
            names.join(" ")
        )
    };
    let mut t = Transcript::default();
    t.section(
        "before",
        &format!(
            "{}{}\n",
            jobs("before"),
            n.apply(&tree(&scratch, &n).join("\n"))
        ),
    );
    let lock = scratch.join("target/held/debug/.cargo-lock");
    let build = fs::File::open(&lock).unwrap();
    build.lock_shared().unwrap();
    let local = format!("{dir}@local");
    let out = s.dibs(["build", &local, "p"]).run();
    t.section(
        "after a prepare, while a build holds one old target: dibs build app@local p",
        &format!(
            "{}{}{}\n",
            said(&s, &n, &out),
            jobs("after"),
            n.apply(&tree(&scratch, &n).join("\n"))
        ),
    );
    drop(build);
    s.command(
        "chmod",
        [
            "700",
            &scratch.join("target/stuck/sub").display().to_string(),
        ],
    )
    .run();
    let out = s.dibs(["build", &local, "p"]).run();
    t.section(
        "after another, once nothing holds it and the stuck one can go: dibs build app@local p",
        &format!(
            "{}{}{}\n",
            said(&s, &n, &out),
            jobs("after"),
            n.apply(&tree(&scratch, &n).join("\n"))
        ),
    );
    snapshot("trees-gc", t.text());
}

#[test]
fn a_git_database_the_machine_lacks_is_sent_ahead_of_the_build() {
    let s = wired();
    let dir = app(&s);
    recipes(&s, PARAMS);
    s.git(".", &["init", "-q", "dep"]);
    s.write("dep/src/lib.rs", "");
    s.git("dep", &["add", "-A"]);
    s.git("dep", &["commit", "-qm", "dep"]);
    let commit = s.git("dep", &["rev-parse", "HEAD"]);
    s.git(
        ".",
        &[
            "clone",
            "-q",
            "--bare",
            "dep",
            "home/.cargo/git/db/dep-0123456789abcdef",
        ],
    );
    s.write(
        "app/Cargo.lock",
        &format!("[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"dep\"\nversion = \"1.0.0\"\nsource = \"git+https://example.invalid/dep.git#{commit}\"\n"),
    );
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "a git dependency"]);
    s.git("app", &["push", "-q", "origin", "HEAD:main"]);
    s.git("app", &["update-ref", "refs/heads/main", "HEAD"]);
    let main = s.git("app", &["rev-parse", "HEAD"]);
    let n = Normal::of(&s)
        .literal(&commit[..8], "<commit>")
        .literal(&main[..12], "<main>")
        .rule(r"local-[0-9a-f]{10}\b", "local-<key>")
        .rule(
            r"(local:[0-9a-f]{7,12})(\+dirty)?-[0-9a-f]{12}\b",
            "local:<sha>$2-<content>",
        );
    let held = |far: &str| {
        let db = s.path(far).join("git/db");
        let has = s
            .command(
                "git",
                [
                    "-C",
                    &db.join("dep-0123456789abcdef").display().to_string(),
                    "cat-file",
                    "-e",
                    &format!("{commit}^{{commit}}"),
                ],
            )
            .run()
            .code
            == 0;
        let mut names: Vec<String> = fs::read_dir(&db)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        format!(
            "the machine's git databases: {}\nit holds the pinned commit: {has}\n",
            names.join(" ")
        )
    };
    let mut t = Transcript::default();
    for (what, reference, far) in [
        (
            "a fetched ref whose lockfile pins a commit the machine lacks, so its step waits",
            "main",
            "far-a",
        ),
        (
            "a working tree of the same lockfile, sent to another machine home",
            "local",
            "far-b",
        ),
        ("again, once the machine has it", "local", "far-b"),
    ] {
        let tree = format!("{dir}@{reference}");
        let args = ["build", tree.as_str(), "p", "--on", "box-a"];
        let out = s
            .dibs(args)
            .env("WIRE_RUN", "1")
            .env("WIRE_FAR_CARGO_HOME", s.p(far))
            .run();
        assert_eq!(out.code, 0, "{what}: {}", out.all());
        t.section(
            &format!("{what}: dibs {}", n.apply(&typed(&args))),
            &format!("{}{}", said(&s, &n, &out), held(far)),
        );
    }
    snapshot("trees-gitdb", t.text());
}

// What seeding sources is for, and the rsync behaviour it rests on: after the sync, a file this
// tree did not change keeps the sibling's time, older than the copied build, and a changed one
// is dated after it.
#[test]
fn after_the_sync_only_changed_files_are_newer_than_the_copied_build() {
    let mut s = Sandbox::new();
    s.set("DIBS_REFLINK", "copy");
    let dir = app(&s);
    recipes(&s, PARAMS);
    s.write("app/src/same.rs", "fn same() {}\n");
    s.write("app/src/edited.rs", "fn before() {}\n");
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "sources"]);
    let first = s.dibs(["build", &format!("{dir}@local"), "p"]).run();
    assert_eq!(first.code, 0, "{}", first.all());
    let sibling = s.p(&format!("scratch/ws/app/local-{}/src", short_hash(&dir)));
    let aged = s
        .command(
            "touch",
            [
                "-d",
                "400 days ago",
                &format!("{sibling}/same.rs"),
                &format!("{sibling}/edited.rs"),
            ],
        )
        .run();
    assert_eq!(aged.code, 0);
    s.git("app", &["worktree", "add", "-q", &s.p("app-topk")]);
    s.write("app-topk/src/edited.rs", "fn after() {}\n");
    let topk = s.p("app-topk");
    let out = s.dibs(["build", &format!("{topk}@local"), "p"]).run();
    assert_eq!(out.code, 0, "{}", out.all());
    let tree = s.path(&format!("scratch/ws/app/local-{}", short_hash(&topk)));
    let age = |file: &str| {
        fs::metadata(tree.join(file))
            .and_then(|m| m.modified())
            .unwrap()
            .elapsed()
            .unwrap_or_default()
            .as_secs()
    };
    assert!(
        age("src/same.rs") > 86400 * 300,
        "an unchanged file keeps the sibling's time: {}",
        out.all()
    );
    assert!(
        age("src/edited.rs") < 3600,
        "a changed file is rewritten and dated now"
    );
    assert_eq!(
        fs::read_to_string(tree.join("src/edited.rs")).unwrap(),
        "fn after() {}\n"
    );
    assert!(
        tree.join(".dibs-used").exists(),
        "the sync does not delete the sweep's marker"
    );
}

/// Master's prepare, as clients that have not updated send it on switch day.
const BASH_PREPARE: &str = include_str!("fixtures/bash-prepare.sh");

#[test]
fn a_bash_prepare_and_a_runner_prepare_of_one_commit_take_turns() {
    let mut s = Sandbox::new();
    let dir = app(&s);
    recipes(&s, PARAMS);
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "recipes"]);
    s.git("app", &["push", "-q", "origin", "HEAD:main"]);
    let main = s.git("app", &["rev-parse", "HEAD"]);
    s.write("bash-prepare.sh", BASH_PREPARE);
    let scratch = s.path("scratch");
    fs::create_dir_all(scratch.join("ws/app")).unwrap();
    let turn = fs::File::create(scratch.join("ws/app/.prepare.lock")).unwrap();
    turn.lock().unwrap();
    let (out, err) = (s.path("bash.out"), s.path("bash.err"));
    let bash = s.spawn(
        s.command("bash", [s.p("bash-prepare.sh")])
            .env("HOME", s.p("home"))
            .env("DIBS_SCRATCH", scratch.display().to_string())
            .streams_to(&out, &err),
    );
    let runner = s.spawn(s.dibs(["build", &format!("{dir}@main"), "p"]));
    s.until_records("the runner's job started", || s.holders() == 1);
    drop(turn);
    assert_eq!(s.wait(bash), 0, "{}", fs::read_to_string(&err).unwrap());
    assert_eq!(s.wait(runner), 0);
    let n = Normal::of(&s).literal(&main[..12], "<main>");
    let said: String = fs::read_to_string(&out)
        .unwrap()
        .lines()
        .map(|l| format!("1| {}\n", n.apply(l)))
        .collect();
    let mut t = Transcript::default();
    t.section(
        "a bash prepare and dibs build app@main p, both of app@main, the repo's turn held until the runner's job starts",
        &format!(
            "{said}{}\n",
            n.apply(&tree(&scratch, &n).join("\n"))
        ),
    );
    snapshot("trees-bash", t.text());
}
