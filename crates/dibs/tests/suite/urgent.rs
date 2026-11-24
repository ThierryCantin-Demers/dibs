use crate::harness::*;

#[test]
fn an_urgent_benchmark_parks_a_shared_job_which_runs_again_after_it() {
    let mut s = Sandbox::new();
    let (work, measure) = (s.gate("work"), s.gate("measure"));
    let order = s.p("order");
    let a = s.spawn(
        s.dibs([
            "--label",
            "work",
            &format!("trap 'exit 0' TERM; echo work >> {order}; {}", work.hold()),
        ])
        .streams_to(&s.path("a-out"), &s.path("a-err")),
    );
    s.until_records("the shared job to start", || s.read("order") == "work\n");
    let u = s.spawn(s.dibs([
        "--bench",
        "--urgent",
        "numbers for the review",
        "--label",
        "measure",
        &format!("echo urgent >> {order}; {}", measure.hold()),
    ]));
    s.until_records("the urgent job to run", || {
        s.read("order").contains("urgent")
    });
    let status = s.status();
    assert_eq!(
        status.lines_with("URGENT: numbers for the review"),
        1,
        "{status}"
    );
    assert_eq!(
        status.lines_with("parked for the urgent job"),
        1,
        "{status}"
    );
    assert_eq!(s.read("order"), "work\nurgent\n", "nothing runs beside it");
    measure.open();
    assert_eq!(s.wait(u), 0);
    s.until_records("the parked job to run again", || {
        s.read("order") == "work\nurgent\nwork\n"
    });
    work.open();
    assert_eq!(s.wait(a), 0, "its caller sees one run that succeeded");
    let told = s.read("a-err");
    assert_eq!(
        told.lines_with("parked for the urgent job"),
        1,
        "its caller is told: {told}"
    );
    s.gone();
}

#[test]
fn an_urgent_benchmark_waits_for_a_running_benchmark_then_goes_before_the_queue() {
    let mut s = Sandbox::new();
    let block = s.gate("block");
    let order = s.p("order");
    let b = s.spawn(s.dibs(["--bench", "--label", "block", &block.hold()]));
    s.held(1);
    let later = s.spawn(s.dibs(["--label", "later", &format!("echo later >> {order}")]));
    s.queued(1);
    let u = s.spawn(s.dibs([
        "--bench",
        "--urgent",
        "deadline",
        "--label",
        "measure",
        &format!("echo urgent >> {order}"),
    ]));
    s.until_records("the urgent job to claim the machine", || {
        s.count("urgent") == 1
    });
    assert_eq!(s.read("order"), "", "a running benchmark is never parked");
    block.open();
    assert_eq!(s.wait(b), 0);
    assert_eq!(s.wait(u), 0);
    assert_eq!(s.wait(later), 0);
    assert_eq!(s.read("order"), "urgent\nlater\n");
    s.gone();
}

#[test]
fn a_second_urgent_job_is_refused_naming_the_first() {
    let mut s = Sandbox::new();
    let first = s.gate("first");
    let u = s.spawn(s.dibs([
        "--bench",
        "--urgent",
        "the first reason",
        "--label",
        "first",
        &first.hold(),
    ]));
    s.held(1);
    s.until_records("the urgent job to claim the machine", || {
        s.count("urgent") == 1
    });
    let second = s
        .dibs([
            "--bench", "--urgent", "another", "--label", "second", "true",
        ])
        .run();
    assert_eq!(second.code, 2, "{}", second.all());
    assert!(
        second.all().contains("from") && second.all().contains("the first reason"),
        "{}",
        second.all()
    );
    first.open();
    assert_eq!(s.wait(u), 0);
    s.gone();
}

#[test]
fn a_benchmark_queued_behind_an_urgent_job_waits_for_it() {
    let mut s = Sandbox::new();
    let measure = s.gate("measure");
    let order = s.p("order");
    let u = s.spawn(s.dibs([
        "--bench",
        "--urgent",
        "deadline",
        "--label",
        "urgent",
        &format!("echo urgent >> {order}; {}", measure.hold()),
    ]));
    s.until_records("the urgent job to run", || s.read("order") == "urgent\n");
    let b = s.spawn(s.dibs([
        "--bench",
        "--label",
        "plain",
        &format!("echo plain >> {order}"),
    ]));
    let shared = s.spawn(s.dibs(["--label", "shared", &format!("echo shared >> {order}")]));
    s.queued(1);
    assert_eq!(s.read("order"), "urgent\n");
    measure.open();
    assert_eq!(s.wait(u), 0);
    assert_eq!(s.wait(b), 0);
    assert_eq!(s.wait(shared), 0);
    assert_eq!(s.read("order").lines().count(), 3);
    s.gone();
}

#[test]
fn a_shared_job_marked_urgent_goes_around_a_queued_benchmark() {
    let mut s = Sandbox::new();
    let (block, measure) = (s.gate("block"), s.gate("measure"));
    let a = s.spawn(s.dibs(["--label", "block", &block.hold()]));
    s.held(1);
    let b = s.spawn(s.dibs(["--bench", "--label", "measure", &measure.hold()]));
    s.queued(1);
    s.gate_taken();
    let look = s.gate("look");
    let quick = s.spawn(
        s.dibs([
            "--urgent",
            "a look before the review",
            "--label",
            "look",
            &look.hold(),
        ])
        .streams_to(&s.path("look-out"), &s.path("look-err")),
    );
    s.held(2);
    let status = s.status();
    let line = status
        .lines()
        .find(|line| line.contains("  look  "))
        .unwrap_or_else(|| panic!("{status}"));
    assert!(
        line.contains("  urgent: a look before the review"),
        "it says why it is urgent: {status}"
    );
    assert_eq!(
        status.lines_with("URGENT:") + status.lines_with("parked for"),
        0,
        "a shared urgent job parks nothing and claims no machine: {status}"
    );
    look.open();
    assert_eq!(s.wait(quick), 0, "{}", s.read("look-err"));
    block.open();
    s.wait(a);
    measure.open();
    assert_eq!(s.wait(b), 0);
    s.gone();
}

#[test]
fn a_frozen_preemptible_job_is_parked_and_runs_again_after_the_urgent_job() {
    let mut s = Sandbox::new();
    let (block, measure, compile) = (s.gate("block"), s.gate("measure"), s.gate("compile"));
    let order = s.p("order");
    let a = s.spawn(s.dibs(["--label", "block", &block.hold()]));
    s.held(1);
    let b = s.spawn(s.dibs(["--bench", "--label", "measure", &measure.hold()]));
    s.queued(1);
    s.gate_taken();
    let p = s.spawn(s.dibs([
        "--preemptible",
        "--label",
        "compile",
        &format!("echo compile >> {order}; {}", compile.hold()),
    ]));
    s.until_records("the compile to start", || s.read("order") == "compile\n");
    block.open();
    s.wait(a);
    s.until_records("the compile to freeze", || {
        s.status().contains("frozen while the benchmark")
    });
    let u = s.spawn(s.dibs([
        "--bench",
        "--urgent",
        "deadline",
        "--label",
        "urgent",
        &format!("echo urgent >> {order}"),
    ]));
    s.until_records("the urgent job to claim the machine", || {
        s.count("urgent") == 1
    });
    measure.open();
    assert_eq!(s.wait(b), 0);
    assert_eq!(s.wait(u), 0);
    s.until_records("the compile to run again", || {
        s.read("order") == "compile\nurgent\ncompile\n"
    });
    compile.open();
    assert_eq!(s.wait(p), 0);
    s.gone();
}

#[test]
fn a_call_that_cannot_wait_gives_up_on_an_urgent_job() {
    let mut s = Sandbox::new();
    let measure = s.gate("measure");
    let u = s.spawn(s.dibs([
        "--bench",
        "--urgent",
        "deadline",
        "--label",
        "urgent",
        &measure.hold(),
    ]));
    s.held(1);
    s.until_records("the urgent job to claim the machine", || {
        s.count("urgent") == 1
    });
    let out = s
        .dibs(["--wait", "1", "--label", "impatient", "echo ran"])
        .run();
    assert_eq!(out.code, 75, "{}", out.all());
    assert!(!out.stdout.contains("ran"));
    measure.open();
    assert_eq!(s.wait(u), 0);
    s.gone();
}

#[cfg(target_os = "linux")]
#[test]
fn an_urgent_job_waits_for_an_orphan_holding_the_lock() {
    let mut s = Sandbox::new();
    let (up, release) = (s.gate("up"), s.gate("release"));
    let o = crate::lock::orphan(&mut s, "-x", &up, &release);
    up.reached();
    let order = s.p("order");
    let u = s.spawn(s.dibs([
        "--bench",
        "--urgent",
        "deadline",
        "--label",
        "urgent",
        &format!("echo urgent >> {order}"),
    ]));
    s.until_records("the urgent job to claim the machine", || {
        s.count("urgent") == 1
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(s.read("order"), "", "nothing runs beside what holds rw");
    release.open();
    s.wait(o);
    assert_eq!(s.wait(u), 0);
    assert_eq!(s.read("order"), "urgent\n");
    s.gone();
}
