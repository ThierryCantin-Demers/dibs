use crate::harness::*;
use std::fs;

const CLOSED: i32 = 79;

fn account() -> String {
    let out = std::process::Command::new("id")
        .arg("-un")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_closed_machine_refuses_new_work_until_it_is_opened() {
    let s = Sandbox::new();
    let close = s
        .dibs(["--close", "--reason", "our own tests until noon"])
        .run();
    assert_eq!(close.code, 0, "{}", close.all());
    let status = s.status();
    assert_eq!(status.lines_with("CLOSED by"), 1, "{status}");
    assert_eq!(status.lines_with("our own tests until noon"), 1, "{status}");
    for args in [
        &["--label", "refused", "echo ran"][..],
        &["--bench", "--label", "refused", "echo ran"],
    ] {
        let out = s.dibs(args.iter().copied()).run();
        assert_eq!(out.code, CLOSED, "{args:?}: {}", out.all());
        assert_eq!(out.all().lines_with("ran"), 0, "{args:?}");
        assert_eq!(out.all().lines_with("our own tests until noon"), 1);
        assert_eq!(out.all().lines_with("--when-open"), 1, "and how to queue");
    }
    assert_eq!(s.dibs(["--open"]).run().code, 0);
    assert_eq!(s.status().lines_with("CLOSED"), 0);
    let out = s.dibs(["--label", "after", "echo ran"]).run();
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.lines_with("ran"), 1);
}

#[test]
fn a_call_that_asks_waits_for_the_machine_to_open() {
    let mut s = Sandbox::new();
    assert_eq!(s.dibs(["--close"]).run().code, 0);
    let job = s.spawn(
        s.dibs(["--when-open", "--label", "overnight", "echo ran"])
            .stdout_to(&s.path("overnight")),
    );
    s.queued(1);
    assert_eq!(s.status().lines_with("overnight"), 1, "it shows as queued");
    assert_eq!(s.dibs(["--open"]).run().code, 0);
    assert_eq!(s.wait(job), 0);
    assert_eq!(s.read("overnight").lines_with("ran"), 1);
    s.gone();
}

#[test]
fn closing_lets_the_running_job_finish_and_waits_for_it() {
    let mut s = Sandbox::new();
    let running = s.gate("running");
    let job = s.spawn(s.dibs(["--label", "running", &running.hold()]));
    s.held(1);
    let close = s.spawn(s.dibs(["--close"]));
    s.until_records("the closure", || s.lockdir().join("closed").exists());
    assert_eq!(
        s.dibs(["--label", "late", "true"]).run().code,
        CLOSED,
        "nothing new starts beside it"
    );
    running.open();
    assert_eq!(s.wait(job), 0, "the running job finished as it would have");
    assert_eq!(s.wait(close), 0, "and the close returned once it had");
    assert_eq!(s.dibs(["--open"]).run().code, 0);
    s.gone();
}

#[test]
fn closing_now_stops_the_running_job() {
    let mut s = Sandbox::new();
    let running = s.gate("running");
    let job = s.spawn(s.dibs(["--label", "running", &running.hold()]));
    s.held(1);
    assert_eq!(s.dibs(["--close", "--now"]).run().code, 0);
    assert_eq!(s.wait(job), CLOSED);
    s.gone();
    assert_eq!(s.dibs(["--open"]).run().code, 0);
}

#[test]
fn a_call_queued_when_the_machine_closes_leaves_the_queue() {
    let mut s = Sandbox::new();
    let bench = s.gate("bench");
    let holder = s.spawn(s.dibs(["--bench", "--label", "holder", &bench.hold()]));
    s.held(1);
    let queued = s.spawn(
        s.dibs(["--label", "queued", "echo ran"])
            .stdout_to(&s.path("queued")),
    );
    let waits = s.spawn(
        s.dibs(["--when-open", "--label", "waits", "echo ran"])
            .stdout_to(&s.path("waits")),
    );
    s.queued(2);
    let close = s.spawn(s.dibs(["--close"]));
    s.until_records("the closure", || s.lockdir().join("closed").exists());
    bench.open();
    assert_eq!(s.wait(holder), 0);
    assert_eq!(s.wait(close), 0);
    assert_eq!(s.wait(queued), CLOSED);
    assert_eq!(s.read("queued").lines_with("ran"), 0);
    s.queued(1);
    assert_eq!(s.dibs(["--open"]).run().code, 0);
    assert_eq!(
        s.wait(waits),
        0,
        "the one that asked to wait ran once it opened"
    );
    assert_eq!(s.read("waits").lines_with("ran"), 1);
    s.gone();
}

#[test]
fn an_excepted_account_carries_on() {
    let s = Sandbox::new();
    assert_eq!(s.dibs(["--close", "--except", &account()]).run().code, 0);
    assert_eq!(s.dibs(["--label", "mine", "true"]).run().code, 0);
    assert_eq!(s.dibs(["--open"]).run().code, 0);
}

#[test]
fn a_bare_file_closes_it_to_everyone() {
    let s = Sandbox::new();
    fs::write(s.lockdir().join("closed"), "").unwrap();
    assert_eq!(s.dibs(["--label", "refused", "true"]).run().code, CLOSED);
    fs::remove_file(s.lockdir().join("closed")).unwrap();
    assert_eq!(s.dibs(["--label", "after", "true"]).run().code, 0);
}
