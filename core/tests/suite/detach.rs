use crate::harness::*;
use regex::Regex;

fn with_queue() -> Sandbox {
    let mut s = Sandbox::new();
    s.set("DIBS_QUEUE", "box@elsewhere");
    s.set("DIBS_QUEUE_LOCAL", "1");
    s.set("DIBS_JOBS_DIR", s.p("jobs"));
    s
}

fn detach(call: Call) -> String {
    call.run().stdout.trim_end().to_string()
}

#[test]
fn a_detached_job_outlives_its_submit_and_keeps_its_result() {
    // The submitting session goes away and the work does not, so the submit has to return while
    // the job is still going, which blocking it on a fifo proves.
    let s = with_queue();
    let g = s.gate("dj");
    let id = detach(s.dibs(["--detach", "--label", "outlives", &format!("{}; echo released", g.hold())]));
    assert!(Regex::new(r"^[0-9]{8}-[0-9]{6}-[0-9]+$").unwrap().is_match(&id), "--detach returns a job id: {id}");
    let state = s.dibs(["--jobs"]).run().stdout.lines().find_map(|l| {
        let f: Vec<_> = l.split_whitespace().collect();
        (f.first() == Some(&id.as_str())).then(|| f.get(2).map(|w| w.to_string()))
    });
    assert_eq!(state.flatten().as_deref(), Some("running"), "and the submit returned while the job is still running");
    g.open();
    until("the job's status", || s.exists(&format!("jobs/{id}/status")));
    assert_eq!(s.read(&format!("jobs/{id}/status")).trim(), "0", "its exit status is kept for later");
    assert_eq!(s.dibs(["--job", &id]).run().stdout.lines().last(), Some("released"), "and so is what it printed");
    assert_eq!(s.dibs(["--job", "no-such-job"]).code(), 2, "an unknown job is refused");
    assert_eq!(s.dibs(["--cancel", &id]).run().all().lines_with("already finished"), 1, "cancelling one that already finished says so");
}

#[test]
fn a_detached_job_can_be_cancelled_while_it_waits() {
    // An agent that realises the command was wrong has to be able to stop it, including while it
    // is still waiting for a machine rather than running on one.
    let s = with_queue();
    let g = s.gate("dk");
    let id = detach(s.dibs(["--detach", "--label", "cancelme", &format!("{}; echo never", g.hold())]));
    assert_eq!(s.dibs(["--cancel", &id]).run().all().lines_with("stopped"), 1, "a job can be stopped");
    let pid: u32 = s.read(&format!("jobs/{id}/pid")).trim().parse().unwrap();
    until("the cancelled job to end", || !alive(pid));
    let out = s.dibs(["--cancel", "nope"]).run();
    assert_eq!(out.code, 2, "cancelling an unknown job is refused");
    assert_eq!(out.stderr.lines_with("--jobs"), 1, "and it says where the ids come from");
}

#[test]
fn a_pid_given_where_a_job_id_goes_names_the_command_that_takes_one() {
    // A holder and a detached job are two namespaces, and --cancel taking a pid found neither: a
    // pid on the default machine read as a missing DIBS_QUEUE.
    let s = Sandbox::new();
    assert_eq!(s.dibs(["--cancel", "12345"]).env("DIBS_QUEUE", "").run().stderr.lines_with("--kill 12345"), 1);
    assert_eq!(
        s.dibs(["--job", "12345"]).env("DIBS_QUEUE", "").run().stderr.lines_with("--status"),
        1,
        "and --job says where a holder shows up instead"
    );
}

#[test]
fn another_agents_detached_job_is_not_yours_to_cancel() {
    // One account runs everyone's jobs there, so the account cannot say whose a job is.
    let s = with_queue();
    let g = s.gate("dm");
    let id = detach(s.dibs(["--detach", "--label", "theirs", &g.hold()]).session("owner"));
    let out = s.dibs(["--cancel", &id]).session("other").run();
    assert_eq!(out.code, 2, "another agent's job is not yours to cancel");
    assert_eq!(out.all().lines_with("belongs to"), 1, "and it says whose it is");
    assert_eq!(s.dibs(["--jobs"]).run().stdout.lines_with("session owner"), 1, "--jobs says who each belongs to");
    assert_eq!(s.dibs(["--cancel", &id, "--anyone"]).session("other").run().stdout.lines_with("stopped"), 1, "--anyone cancels it");
}

#[test]
fn detaching_needs_a_queue_and_reading_one_takes_on() {
    let mut s = with_queue();
    s.machines(&format!("[machine.here]\nssh = \"here\"\nhostname = \"{}\"\n", hostname()));
    let no_queue = |args: &[&str]| s.dibs(args).env("DIBS_QUEUE", "");
    // Running it here, to die with the session anyway, would only pretend to detach.
    assert_eq!(no_queue(&["--detach", "true"]).code(), 2, "with no queue it refuses rather than pretending");
    detach(s.dibs(["--detach", "--label", "listed", "true"]));
    assert_eq!(no_queue(&["--jobs", "--on", "here"]).run().stdout.lines_matching("^ID "), 1, "reading a job takes --on, the same as submitting one");
    assert_eq!(no_queue(&["--jobs"]).run().stderr.lines_with("no --on"), 1, "and with neither it says both ways of naming one");
}

#[test]
fn bench_and_detach_together_are_refused_and_the_named_form_works() {
    // Both used to set the mode, so the order on the line decided which one was silently lost.
    let mut s = with_queue();
    for order in [["--bench", "--detach"], ["--detach", "--bench"]] {
        let out = s.dibs([order[0], order[1], "--label", "bd", "true"]).run();
        assert_eq!(out.code, 2, "{order:?} is refused rather than half-honoured");
        assert_eq!(out.all().lines_with("does not take the lock"), 1, "and it says the lock is not taken");
        assert_eq!(out.all().lines_with("detach 'dibs --bench"), 1, "and it names the form that does");
    }
    assert_eq!(s.dibs(["--jobs"]).run().stdout.lines_with(" bd "), 0, "no job was submitted by either");
    // A named form nobody exercised is how a helpful message turns into a wrong one.
    let g = s.gate("db");
    let inner = format!("{DIBS} --bench --label inner-bench '{}'", g.hold());
    let _submit = s.spawn(s.dibs(["--detach", "--label", "inner", &inner]));
    s.held(1);
    let status = s.status();
    let after_busy = status.lines().skip_while(|l| !l.contains("BUSY, benchmark"));
    assert_eq!(after_busy.filter(|l| l.contains("inner-bench")).count(), 1, "the form the refusal names does take the lock");
    g.open();
    s.gone();
}

#[test]
fn what_describes_a_run_is_refused_with_detach() {
    // It went to a queue that had no idea what to do with it, and the job ran as if never given.
    let s = with_queue();
    for flags in [&["--device", "gpu:none"][..], &["--new-series"], &["--wait", "5"], &["--max", "60"]] {
        let mut args = vec!["--detach"];
        args.extend_from_slice(flags);
        args.push("true");
        assert_eq!(s.dibs(&args).code(), 2, "{flags:?} with --detach is refused");
    }
    // --jobs reads one machine, and a job scattered across the pool is one nobody can find again.
    let id = detach(s.dibs(["--detach", "--label", "routed", "true"]).env("DIBS_HOST", ""));
    assert_eq!(id.lines_matching("^[0-9]{8}-"), 1, "a detached job is not routed away from its queue");
}
