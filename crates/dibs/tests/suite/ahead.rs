use crate::harness::*;
use std::{fs, thread, time::Duration};

fn frozen(s: &Sandbox, pid: u32) -> bool {
    fs::read_to_string(s.lockdir().join(format!("ahead.{pid}")))
        .is_ok_and(|said| said.split_whitespace().count() == 2)
}

#[test]
fn a_preemptible_job_runs_ahead_of_a_queued_benchmark_and_is_frozen_while_it_runs() {
    let mut s = Sandbox::new();
    let (block, measure, compile) = (s.gate("block"), s.gate("measure"), s.gate("compile"));
    let a = s.spawn(s.dibs(["--label", "block", &block.hold()]));
    s.held(1);
    let b = s.spawn(s.dibs(["--bench", "--label", "measure", &measure.hold()]));
    s.queued(1);
    s.gate_taken();
    let p = s.spawn(s.dibs([
        "--preemptible",
        "--max",
        "3",
        "--label",
        "compile",
        &compile.hold(),
    ]));
    s.held(2);
    assert_eq!(
        s.status().lines_with("preemptible"),
        1,
        "it starts beside the block, ahead of the benchmark"
    );
    let pid = s.pid_of("compile");
    block.open();
    s.wait(a);
    s.until_records("the compile to freeze", || frozen(&s, pid));
    s.until_records("the benchmark to start", || {
        s.status().contains("BUSY, benchmark")
    });
    assert_eq!(
        s.status().lines_with("frozen while the benchmark"),
        1,
        "status says why it is quiet"
    );
    // Longer than its --max, which time frozen does not count against.
    thread::sleep(Duration::from_secs(4));
    measure.open();
    assert_eq!(s.wait(b), 0);
    s.until_records("the compile to thaw", || !frozen(&s, pid));
    compile.open();
    assert_eq!(s.wait(p), 0, "it finishes where it stopped");
    s.gone();
}

#[test]
fn a_preemptible_job_queued_before_a_benchmark_is_waited_for_not_frozen() {
    let mut s = Sandbox::new();
    let (block, compile) = (s.gate("block"), s.gate("compile"));
    let a = s.spawn(s.dibs(["--label", "block", &block.hold()]));
    s.held(1);
    let p = s.spawn(s.dibs(["--preemptible", "--label", "compile", &compile.hold()]));
    s.held(2);
    let b = s.spawn(
        s.dibs(["--bench", "--label", "measure", "echo ran"])
            .stdout_to(&s.path("measured")),
    );
    s.queued(1);
    block.open();
    s.wait(a);
    s.gate_taken();
    assert_eq!(
        s.count("pause"),
        0,
        "it asks nothing of a job queued before it"
    );
    assert!(
        !s.read("measured").contains("ran"),
        "the benchmark waits for a job queued before it"
    );
    compile.open();
    assert_eq!(s.wait(p), 0, "which it never froze");
    assert_eq!(s.wait(b), 0);
    assert_eq!(s.read("measured").lines_with("ran"), 1);
    s.gone();
}

#[test]
fn only_a_shared_job_with_nothing_to_strand_is_preemptible() {
    let s = Sandbox::new();
    for args in [
        &["--bench", "--preemptible", "true"][..],
        &["--hold", "--preemptible", "true"],
        &["--preemptible", "--port", "api", "true"],
    ] {
        let out = s.dibs(args.iter().copied()).run();
        assert_ne!(out.code, 0, "{args:?}");
        assert_eq!(out.stderr.lines_with("--preemptible is for"), 1, "{args:?}");
    }
}

#[test]
#[cfg(target_os = "linux")]
fn a_preemptible_job_never_runs_beside_a_benchmark_from_before_preemption() {
    // Such a benchmark's runner takes `rw` exclusive and knows nothing of `ahead`.
    let mut s = Sandbox::new();
    let rw = s.lockdir().join("rw");
    let old = s.gate("old");
    let bench = s.spawn(s.sh(&format!("flock -x {} -c '{}'", rw.display(), old.hold())));
    s.until_records("the old benchmark to take rw", || {
        fs::File::open(&rw).is_ok_and(|f| {
            let free = f.try_lock_shared().is_ok();
            let _ = f.unlock();
            !free
        })
    });
    assert_eq!(
        s.dibs(["--preemptible", "--wait", "1", "--label", "compile", "true"])
            .code(),
        75,
        "it does not start beside one running"
    );
    old.open();
    s.wait(bench);

    let compile = s.gate("compile");
    let p = s.spawn(s.dibs(["--preemptible", "--label", "compile", &compile.hold()]));
    s.held(1);
    assert_ne!(
        s.sh(&format!("flock -x -w 1 {} true", rw.display())).code(),
        0,
        "and one that arrives waits for it"
    );
    compile.open();
    s.wait(p);
}
