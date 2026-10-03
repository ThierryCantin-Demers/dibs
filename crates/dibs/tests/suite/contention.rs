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
    let mode = if bench { "bench" } else { "shared" };
    match half {
        Half::Payload => {
            let script = s.machine_script(&[
                ("MODE", mode),
                ("LABEL", label),
                ("CMD", command),
                ("NO_WATCH", "1"),
            ]);
            s.spawn(s.command("bash", [script]))
        }
        Half::Runner => {
            let mut args = vec!["--label", label, command];
            if bench {
                args.insert(0, "--bench");
            }
            s.spawn(s.dibs(args))
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
