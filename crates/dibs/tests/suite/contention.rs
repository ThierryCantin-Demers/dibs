//! A runner and a bash payload on one machine at once, as on the day clients switch: each has to
//! exclude the other through the lock files alone.

use crate::harness::*;

/// Which half of dibs takes the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    Payload,
    Runner,
}

/// One locked job, started on the half named.
fn start(s: &mut Sandbox, half: Half, bench: bool, label: &str, command: &str) -> Job {
    let call = call(s, half, bench, label, command);
    s.spawn(call)
}

/// The same, not yet started.
fn call(s: &Sandbox, half: Half, bench: bool, label: &str, command: &str) -> Call {
    let mode = if bench { "bench" } else { "shared" };
    match half {
        Half::Payload => {
            let script = s.machine_script(&[
                ("MODE", mode),
                ("LABEL", label),
                ("CMD", command),
                ("NO_WATCH", "1"),
            ]);
            s.command("bash", [script])
        }
        Half::Runner => {
            let mut args = vec!["--label", label, command];
            if bench {
                args.insert(0, "--bench");
            }
            s.dibs(args)
        }
    }
}

/// The half a record's pid is, by its command line.
fn half_of(s: &Sandbox, kind: &str) -> Half {
    let pid = &s.records(kind)[0][1];
    let args = s.command("ps", ["-o", "args=", "-p", pid]).run().stdout;
    match args.contains("__runner") {
        true => Half::Runner,
        false => Half::Payload,
    }
}

/// The first half holds the lock in one mode while the second queues in the other, and the second
/// runs only once the first has let go.
fn excludes(first: Half, second: Half, first_bench: bool) {
    let mut s = Sandbox::new();
    let (up, hold, after) = (s.gate("up"), s.gate("hold"), s.gate("after"));
    let holder = start(
        &mut s,
        first,
        first_bench,
        "first",
        &format!("{}; {}", up.signal(), hold.hold()),
    );
    up.reached();
    s.held(1);
    assert_eq!(half_of(&s, "holder"), first, "the holder is the half meant");
    let waiter = start(&mut s, second, !first_bench, "second", &after.signal());
    s.queued(1);
    assert_eq!(half_of(&s, "waiting"), second, "and so is the waiter");
    assert_eq!(
        (s.holders(), s.waiters()),
        (1, 1),
        "{second:?} waits while {first:?} holds the lock"
    );
    hold.open();
    after.reached();
    assert_eq!(s.wait(holder), 0, "{first:?} ends as its command did");
    assert_eq!(s.wait(waiter), 0, "and {second:?} runs once it has");
    s.gone();
}

#[test]
fn a_payloads_benchmark_holds_off_a_runners_shared_job() {
    excludes(Half::Payload, Half::Runner, true);
}

#[test]
fn a_payloads_shared_job_holds_off_a_runners_benchmark() {
    excludes(Half::Payload, Half::Runner, false);
}

#[test]
fn a_runners_benchmark_holds_off_a_payloads_shared_job() {
    excludes(Half::Runner, Half::Payload, true);
}

#[test]
fn a_runners_shared_job_holds_off_a_payloads_benchmark() {
    excludes(Half::Runner, Half::Payload, false);
}

/// One half's benchmark queued behind a shared holder keeps the gate, so the other half's shared
/// job waits behind it, though it could share the lock with the holder.
fn a_queued_benchmark_keeps_the_gate(bench: Half, shared: Half) {
    let mut s = Sandbox::new();
    let (up, hold, measured, measuring, after) = (
        s.gate("up"),
        s.gate("hold"),
        s.gate("measured"),
        s.gate("measuring"),
        s.gate("after"),
    );
    let holder = start(
        &mut s,
        shared,
        false,
        "holder",
        &format!("{}; {}", up.signal(), hold.hold()),
    );
    up.reached();
    s.held(1);
    let measure = format!("{}; {}", measured.signal(), measuring.hold());
    let queued = start(&mut s, bench, true, "measure", &measure);
    s.queued(1);
    let late = start(&mut s, shared, false, "late", &after.signal());
    s.queued(2);
    assert_eq!(
        (s.holders(), s.waiters()),
        (1, 2),
        "{shared:?}'s shared job waits behind {bench:?}'s queued benchmark"
    );
    hold.open();
    assert_eq!(s.wait(holder), 0);
    measured.reached();
    assert_eq!(
        (s.holders(), s.waiters()),
        (1, 1),
        "the benchmark goes before it"
    );
    measuring.open();
    after.reached();
    assert_eq!(s.wait(queued), 0);
    assert_eq!(s.wait(late), 0);
    s.gone();
}

#[test]
fn a_payloads_queued_benchmark_keeps_out_a_runners_shared_job() {
    a_queued_benchmark_keeps_the_gate(Half::Payload, Half::Runner);
}

#[test]
fn a_runners_queued_benchmark_keeps_out_a_payloads_shared_job() {
    a_queued_benchmark_keeps_the_gate(Half::Runner, Half::Payload);
}

/// A quick job of one half goes around the other half's queued benchmark, which it can only do
/// by reading that half's waiting record.
fn a_quick_job_goes_around(bench: Half, quick: Half) {
    let mut s = Sandbox::new();
    s.history("shared\tquickie\t1\nshared\tquickie\t1\nshared\tquickie\t1\n");
    let (up, hold, measuring, around, quick_hold) = (
        s.gate("up"),
        s.gate("hold"),
        s.gate("measuring"),
        s.gate("around"),
        s.gate("quick"),
    );
    let holder = start(
        &mut s,
        Half::Runner,
        false,
        "holder",
        &format!("{}; {}", up.signal(), hold.hold()),
    );
    up.reached();
    s.held(1);
    let queued = start(&mut s, bench, true, "measure", &measuring.hold());
    s.queued(1);
    let around_and_hold = format!("{}; {}", around.signal(), quick_hold.hold());
    let goes = call(&s, quick, false, "quickie", &around_and_hold)
        .env("DIBS_PATIENCE", "600")
        .env("DIBS_QUICK", "5");
    let goes = s.spawn(goes);
    around.reached();
    assert_eq!(
        (s.holders(), s.waiters()),
        (2, 1),
        "{quick:?}'s quick job went around {bench:?}'s queued benchmark"
    );
    quick_hold.open();
    assert_eq!(s.wait(goes), 0);
    hold.open();
    assert_eq!(s.wait(holder), 0);
    measuring.open();
    assert_eq!(s.wait(queued), 0);
    s.gone();
}

#[test]
fn a_runners_quick_job_goes_around_a_payloads_queued_benchmark() {
    a_quick_job_goes_around(Half::Payload, Half::Runner);
}

#[test]
fn a_payloads_quick_job_goes_around_a_runners_queued_benchmark() {
    a_quick_job_goes_around(Half::Runner, Half::Payload);
}

#[test]
fn a_runners_release_takes_nothing_from_a_payload_holding_the_lock() {
    let mut s = Sandbox::new();
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let holder = start(
        &mut s,
        Half::Payload,
        true,
        "held",
        &format!("{}; {}", up.signal(), hold.hold()),
    );
    up.reached();
    s.held(1);
    let released = s.dibs(["--release"]).run();
    assert_eq!(released.code, 0, "{}", released.all());
    assert_eq!(s.holders(), 1, "the payload still holds the lock");
    assert_eq!(
        s.log().lines_with("\treclaimed\t"),
        0,
        "and nothing was reclaimed"
    );
    hold.open();
    assert_eq!(s.wait(holder), 0, "its job ran to its end");
}

#[test]
fn a_runners_kill_stops_a_payload_holder_and_its_job() {
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let command = format!(
        "echo $$ > {}; {}; {}",
        s.p("job.pid"),
        up.signal(),
        never.hold()
    );
    let holder = start(&mut s, Half::Payload, false, "held", &command);
    up.reached();
    s.held(1);
    let job: u32 = s.read("job.pid").trim().parse().unwrap();
    let pid = s.records("holder")[0][1].clone();
    let killed = s.dibs(["--kill", &pid, "--anyone"]).run();
    assert_eq!(killed.code, 0, "{}", killed.all());
    s.wait(holder);
    s.gone();
    until("the payload's job to go with it", || !alive(job));
}
