use crate::harness::*;

#[test]
fn every_job_says_whose_it_is() {
    // Agents are told apart by their session, and a session with no title on disk still has an
    // id. The point of carrying it is that the user can go and ask that agent what it was doing.
    let mut s = Sandbox::new();
    let (a1, a2) = (s.gate("a1"), s.gate("a2"));
    let owned = s.spawn(s.dibs(["--bench", "--label", "owned", &a1.hold()]).session("deadbeef"));
    s.held(1);
    assert_eq!(s.status().lines_with("from session deadbeef"), 1, "a holder says whose it is");
    let behind = s.spawn(s.dibs(["--label", "behind-it", &a2.hold()]).session("cafe"));
    s.queued(1);
    let status = s.status();
    assert_eq!(status.lines_with("from session cafe"), 1, "and so does one still in the queue");
    assert_eq!(status.lines_with("read -r _ <"), 2, "the queue shows what each will run");
    a1.open();
    s.wait(owned);
    a2.open();
    s.wait(behind);
    assert_eq!(s.dibs(["--log", "20"]).run().stdout.lines_with("session deadbeef"), 2, "the log says who ran what");
}

fn last_two(s: &Sandbox, call: Call) -> String {
    call.run();
    s.dibs(["--log", "2"]).run().stdout
}

#[test]
fn a_caller_with_no_session_is_named_for_what_it_is() {
    let s = Sandbox::new();
    let log = last_two(&s, s.dibs(["--label", "byhand", "echo hi"]).no_session());
    assert_eq!(log.lines_with("at a shell"), 2, "a shell that is no agent says so instead");
    // Codex publishes no session id and runs every session through one shell, so it arrived as
    // the unix user and looked like a person at a terminal.
    let log = last_two(&s, s.dibs(["--label", "bycodex", "echo hi"]).no_session().env("CODEX_SHELL", "1"));
    assert_eq!(log.lines_with("a Codex session"), 2, "a runtime with no session id still says which runtime");
    // The one thing that works for a runtime dibs has never heard of.
    let log = last_two(&s, s.dibs(["--label", "byname", "echo hi"]).no_session().env("DIBS_AGENT", "sweeping reduce"));
    assert_eq!(log.lines_with("sweeping reduce"), 2, "a session that names itself is taken at its word");
    let log = last_two(&s, s.dibs(["--label", "byboth", "echo hi"]).session("guess").env("DIBS_AGENT", "said so"));
    assert_eq!(log.lines_with("said so"), 2, "which outranks a runtime that guessed");
}

#[test]
fn a_peek_that_costs_something_says_so() {
    let s = Sandbox::new();
    let peek = |cmd: &str| s.dibs(["--peek", cmd]).env("DIBS_PEEK_WARN", "1");
    assert_eq!(peek("echo fine").run().all(), "fine\n", "a cheap peek says nothing");
    let out = peek("timeout 1.5 python3 -c 'while True: pass'").session("peeker").run().all();
    assert_eq!(out.lines_with("ran with no lock"), 1, "a costly one warns about the lock it skipped");
    assert_eq!(s.log().lines_with("peek-slow"), 1, "and is recorded as peek-slow");
    let log = s.dibs(["--log", "5"]).run().stdout;
    assert_eq!(
        log.lines().filter(|l| l.contains("peek-slow") && l.contains("session peeker")).count(),
        1,
        "and the peek-slow line names who did it"
    );
}

#[test]
fn every_event_names_the_job_it_belongs_to() {
    let s = Sandbox::new();
    let out = s.dibs(["--label", "jobcol", "true"]).run();
    let job = job_id(&out.stderr);
    let events: Vec<_> = s
        .log()
        .lines()
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|f| f.get(4) == Some(&"jobcol") && f.get(11) == Some(&job.as_str()))
        .map(|f| f[1].to_string())
        .collect();
    assert_eq!(events, ["arrived", "finished"], "every event names the job it belongs to, as the trailer does");
    let rendered = s.dibs(["--log", "3"]).run().stdout;
    assert_eq!(rendered.lines().next().map(|l| l.contains("WHEN")), Some(true), "--log renders a header");
    assert_eq!(rendered.lines_matching(&format!("finished .* {job} ")), 1, "and shows the job");
    // The one mechanism that runs beside a measurement leaves a row saying so.
    s.dibs(["--peek", "true"]).run();
    assert!(s.log().lines().last().unwrap().contains("\tpeek\t"), "a peek is an event");
}
