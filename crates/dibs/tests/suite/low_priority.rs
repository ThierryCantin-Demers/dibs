use crate::harness::*;
use dibs_format::Mode;

fn idle(s: &Sandbox, label: &str, command: &str) -> Call {
    s.dibs(["--low-priority", "--label", label, command])
        .env("DIBS_IDLE_SETTLE", "0")
}

#[test]
fn a_low_priority_job_waits_for_an_idle_machine_and_is_parked_for_a_benchmark() {
    let mut s = Sandbox::new();
    let (busy, long, measure) = (s.gate("busy"), s.gate("long"), s.gate("measure"));
    let order = s.p("order");
    let b = s.spawn(s.dibs([
        "--label",
        "busy",
        &format!("echo busy >> {order}; {}", busy.hold()),
    ]));
    s.until_records("the shared job to start", || s.read("order") == "busy\n");
    let i = s.spawn(
        idle(
            &s,
            "long",
            &format!("trap 'exit 0' TERM; echo long >> {order}; {}", long.hold()),
        )
        .streams_to(&s.path("i-out"), &s.path("i-err")),
    );
    s.queued(1);
    assert_eq!(s.read("order"), "busy\n", "it waits while another job runs");
    let status = s.status();
    assert_eq!(
        status.lines_with("waits for an idle machine, behind busy from"),
        1,
        "it names the job it waits behind: {status}"
    );
    busy.open();
    assert_eq!(s.wait(b), 0);
    s.until_records("the idle machine to start it", || {
        s.read("order") == "busy\nlong\n"
    });
    let m = s.spawn(s.dibs([
        "--bench",
        "--label",
        "measure",
        &format!("echo measure >> {order}; {}", measure.hold()),
    ]));
    s.until_records("the benchmark to run once it is parked", || {
        s.read("order") == "busy\nlong\nmeasure\n"
    });
    assert_eq!(s.holders(), 1, "the benchmark has the machine alone");
    measure.open();
    assert_eq!(s.wait(m), 0);
    s.until_records("the parked job to run again", || {
        s.read("order") == "busy\nlong\nmeasure\nlong\n"
    });
    long.open();
    assert_eq!(s.wait(i), 0, "its caller sees one run that succeeded");
    let told = s.read("i-err");
    assert_eq!(told.lines_with("parked for measure"), 1, "{told}");
    s.gone();
}

#[test]
fn a_transfer_does_not_keep_a_low_priority_job_waiting() {
    let mut s = Sandbox::new();
    let (sending, run) = (s.gate("sending"), s.gate("run"));
    s.write(
        "request",
        &request_frame(Mode::Rsh, "send", &sending.hold()),
    );
    let send = s.spawn(s.sh(&format!(
        "dibs __runner serve {RUNNER_HASH} < {} > /dev/null",
        s.p("request")
    )));
    s.held(1);
    let order = s.p("order");
    let i = s.spawn(idle(
        &s,
        "long",
        &format!("echo long >> {order}; {}", run.hold()),
    ));
    s.until_records("it to start beside the transfer", || {
        s.read("order") == "long\n"
    });
    sending.open();
    s.wait(send);
    run.open();
    assert_eq!(s.wait(i), 0);
    assert_eq!(s.read("order"), "long\n", "it ran once, never parked");
    s.gone();
}

#[test]
fn low_priority_jobs_do_not_wait_for_each_other() {
    let mut s = Sandbox::new();
    let (one, two) = (s.gate("one"), s.gate("two"));
    let a = s.spawn(idle(&s, "one", &one.hold()));
    let b = s.spawn(idle(&s, "two", &two.hold()));
    s.held(2);
    one.open();
    two.open();
    assert_eq!(s.wait(a), 0);
    assert_eq!(s.wait(b), 0);
    s.gone();
}

#[test]
fn low_priority_is_refused_for_what_cannot_run_again_from_the_start() {
    let s = Sandbox::new();
    for flags in [
        &["--bench"][..],
        &["--urgent", "now"],
        &["--preemptible"],
        &["--hold"],
    ] {
        let mut words = vec!["--low-priority"];
        words.extend_from_slice(flags);
        words.push("true");
        let out = s.dibs(words).run();
        assert_eq!(out.code, 2, "{flags:?}: {}", out.all());
        assert!(out.all().contains("--low-priority"), "{}", out.all());
    }
}
