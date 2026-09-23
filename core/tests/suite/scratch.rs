use crate::harness::*;
use std::fs;

#[test]
fn a_job_gets_the_scratch_it_was_given() {
    // --check tells whoever fixes a broken machine to set DIBS_SCRATCH, so it has to be the
    // variable the code actually reads.
    let s = Sandbox::new();
    let scr = s.p("scr");
    let out = s.dibs(["--label", "scr", "echo $DIBS_SCRATCH"]).env("DIBS_SCRATCH", &scr).run().stdout;
    assert_eq!(out.lines().last(), Some(scr.as_str()));
}

#[test]
fn a_job_can_see_a_toolchain_the_login_shell_would_have_set_up() {
    let s = Sandbox::new();
    let on_path = s.dibs(["--label", "path-cargo", r#"case ":$PATH:" in *":$HOME/.cargo/bin:"*) echo yes ;; *) echo no ;; esac"#]);
    assert_eq!(on_path.run().stdout.trim_end(), "yes", "cargo installed by rustup is on the path");
    let twice = s.dibs(["--label", "path-twice", r#"printf "%s\n" "$PATH" | tr ":" "\n" | sort | uniq -d | grep -c cargo"#]);
    assert_eq!(twice.run().stdout.trim_end(), "0", "and is not added twice");
}

/// A scratch of its own, since a sweep is judged by what it removed.
fn scratch_to_sweep(s: &Sandbox) -> String {
    for d in ["ws/demo/stale", "ws/demo/fresh", "target/demo", "target/demo-arm1", "jobs/20260101-1", "tmp/left", "byhand"] {
        fs::create_dir_all(s.path(&format!("gc/{d}"))).unwrap();
    }
    s.write("gc/target/demo-arm1/blob", &"x".repeat(300_000));
    let touch = |when: &str, rel: &[&str]| {
        let paths: Vec<String> = rel.iter().map(|r| s.p(&format!("gc/{r}"))).collect();
        let mut args = vec!["-d", when];
        args.extend(paths.iter().map(String::as_str));
        assert_eq!(s.command("touch", args).code(), 0);
    };
    touch("30 days ago", &["ws/demo/stale/.dibs-used", "jobs/20260101-1", "tmp/left"]);
    touch("9 days ago", &["target/demo-arm1/.dibs-used"]);
    touch("now", &["ws/demo/fresh/.dibs-used", "target/demo/.dibs-used"]);
    s.p("gc")
}

#[test]
fn a_dry_run_names_what_is_past_its_clock_and_removes_nothing() {
    let s = Sandbox::new();
    let g = scratch_to_sweep(&s);
    let out = s.dibs(["--gc", "--dry-run"]).env("DIBS_SCRATCH", &g).run().all();
    assert_eq!(out.lines_matching("ws/demo/stale .* would remove"), 1, "a dry run names what is past its clock");
    assert!(s.exists("gc/ws/demo/stale"), "and removes nothing");
    // Five days for a cache against fourteen for a tree: a cache is refilled by a compiler.
    assert_eq!(out.lines_matching("target/demo-arm1 .* would remove"), 1, "a cache is judged by its own shorter clock");
    assert_eq!(out.lines_matching("target/demo  .* would remove"), 0, "and one used today is left out of it");
    assert_eq!(out.lines_with("byhand"), 1, "what dibs did not put there is listed");
}

#[test]
fn a_sweep_removes_only_what_dibs_made_and_nobody_used() {
    let s = Sandbox::new();
    let g = scratch_to_sweep(&s);
    let out = s.dibs(["--gc"]).env("DIBS_SCRATCH", &g).run().all();
    assert!(!s.exists("gc/ws/demo/stale"), "the sweep removes a stale worktree");
    assert!(s.exists("gc/ws/demo/fresh"), "and keeps one in use");
    assert!(!s.exists("gc/target/demo-arm1"), "and removes a stale cache");
    assert!(s.exists("gc/target/demo"), "and keeps the one built into today");
    // A directory somebody wrote by hand may be the only copy of what they are working on.
    assert!(s.exists("gc/byhand"), "and never what it did not make");
    assert_eq!(out.lines_with("reclaimed "), 1, "it says how much came back");
    assert_eq!(out.lines_with(" gc  dibs-gc "), 1, "and it is a job like any other");
    s.command("touch", ["-d", "3 days ago", &s.p("gc/ws/demo/fresh/.dibs-used")]).run();
    let lowered = s.dibs(["--gc", "--days", "2", "--dry-run"]).env("DIBS_SCRATCH", &g).run().all();
    assert_eq!(lowered.lines_matching("ws/demo/fresh .* would remove"), 1, "--days lowers every clock to it");
}

#[test]
fn gc_takes_its_own_flags_only() {
    let s = Sandbox::new();
    assert_eq!(s.dibs(["--gc", "echo no"]).code(), 2, "--gc takes no command");
    assert_eq!(s.dibs(["--dry-run", "echo no"]).code(), 2, "and --dry-run belongs to it");
    assert_eq!(s.dibs(["--gc", "--days", "x"]).code(), 2, "--days takes a number");
}

#[test]
fn a_sweep_waits_for_a_benchmark_rather_than_running_beside_it() {
    // Deleting gigabytes is as much IO as writing them, which is the whole reason it takes a lock.
    let mut s = Sandbox::new();
    let g = scratch_to_sweep(&s);
    let b = s.gate("b");
    let bench = s.spawn(s.dibs(["--bench", "--label", "gc-bench", &b.hold()]));
    s.held(1);
    assert_eq!(s.dibs(["--gc", "--wait", "1"]).env("DIBS_SCRATCH", &g).code(), 75);
    b.open();
    s.wait(bench);
}
