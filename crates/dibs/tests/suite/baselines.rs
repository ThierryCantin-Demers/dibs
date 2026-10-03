//! What a call costs; `cargo test --test suite -- --ignored --test-threads=1 baselines` runs it.

use crate::harness::*;
use crate::wire::wired;
use std::fs;
use std::io::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

const REPEATS: usize = 10;

/// Printed past the harness's capture, so the numbers show without --nocapture.
fn say(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// The bytes the recording ssh was handed: its arguments and everything on its stdin.
fn bytes_sent(s: &Sandbox) -> usize {
    fs::read_dir(s.path("wire"))
        .unwrap()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            [".argv", ".frame", ".rest"]
                .iter()
                .any(|kind| name.ends_with(kind))
        })
        .map(|e| e.metadata().map(|m| m.len() as usize).unwrap_or(0))
        .sum()
}

#[test]
#[ignore]
fn bytes_sent_per_call() {
    let s = wired();
    say("bytes sent per call, over ssh to box-a: arguments and all of ssh's stdin");
    let calls: [&[&str]; 5] = [
        &["--on", "box-a", "--label", "cost", "true"],
        &["--on", "box-a", "--bench", "--label", "cost", "true"],
        &["--on", "box-a", "--peek", "true"],
        &["--on", "box-a", "--status"],
        &["--on", "box-a", "--status", "--json"],
    ];
    for args in calls {
        for e in fs::read_dir(s.path("wire")).unwrap().flatten() {
            let _ = fs::remove_file(e.path());
        }
        let out = s
            .dibs(args)
            .env("WIRE_RUN", "1")
            .env("WIRE_STDIN", "1")
            .run();
        assert_eq!(out.code, 0, "{args:?}: {}", out.all());
        say(&format!(
            "  {:>7} B  dibs {}",
            bytes_sent(&s),
            args.join(" ")
        ));
    }
}

/// Seconds since the epoch, as the job's own `date +%s.%N` prints them.
fn epoch_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// Milliseconds from a call to its command's first line, and to its exit.
struct Timing {
    start: f64,
    exit: f64,
}

fn timed(call: Call, started: &str) -> Timing {
    let t0 = epoch_now();
    let out = call.run();
    let t1 = epoch_now();
    assert_eq!(out.code, 0, "{}", out.all());
    let at: f64 = fs::read_to_string(started).unwrap().trim().parse().unwrap();
    Timing {
        start: (at - t0) * 1000.0,
        exit: (t1 - t0) * 1000.0,
    }
}

#[test]
#[ignore]
fn time_to_job_start_per_call() {
    let s = wired();
    let mark = s.p("started");
    let cmd = format!("date +%s.%N > {mark}");
    say(&format!(
        "time from the call to its job's first line, median of {REPEATS}, with the time to its exit"
    ));
    let kinds: [(&str, &[&str], &str); 4] = [
        ("shared, on this computer", &[], "DIBS_LOCAL"),
        ("bench, on this computer", &["--bench"], "DIBS_LOCAL"),
        ("shared, over ssh", &["--on", "box-a"], "WIRE_RUN"),
        ("bench, over ssh", &["--on", "box-a", "--bench"], "WIRE_RUN"),
    ];
    for (what, flags, switch) in kinds {
        let call = || {
            let args: Vec<&str> = flags
                .iter()
                .copied()
                .chain(["--label", "start", &cmd])
                .collect();
            s.dibs(args).env(switch, "1")
        };
        let (mut start, mut exit) = (Vec::new(), Vec::new());
        for _ in 0..REPEATS {
            let Timing { start: a, exit: b } = timed(call(), &mark);
            start.push(a);
            exit.push(b);
        }
        say(&format!(
            "  {:>6.0} ms to start  {:>6.0} ms to exit  {what}",
            median(start),
            median(exit)
        ));
    }
}

/// The CPU `script` spent in its children, in seconds, as bash's `times` reports it.
fn children_cpu(s: &Sandbox, script: &str) -> f64 {
    let out = s.sh(&format!("{script}\ntimes")).run().stdout;
    let line = out.lines().last().unwrap_or_default();
    line.split_whitespace()
        .map(|t| {
            let (m, rest) = t.split_once('m').unwrap_or(("0", t));
            m.parse::<f64>().unwrap_or(0.0) * 60.0
                + rest.trim_end_matches('s').parse::<f64>().unwrap_or(0.0)
        })
        .sum()
}

#[test]
#[ignore]
fn cpu_per_status_tick_with_a_thirty_process_holder() {
    let mut s = Sandbox::new();
    let gate = s.gate("wide");
    let wide = format!(
        "for i in $(seq 30); do cat {} & done; wait",
        gate.path.display()
    );
    let holder = s.spawn(s.dibs(["--label", "wide", &wide]));
    s.held(1);
    until("the holder's thirty processes", || {
        let tree = s
            .sh(&format!("pgrep -f '^cat {}' | wc -l", gate.path.display()))
            .run()
            .stdout;
        tree.trim().parse::<usize>().unwrap_or(0) >= 30
    });
    say("CPU per status of a holder with thirty processes under it, on this computer");
    let calls = 10;
    let fresh = children_cpu(
        &s,
        &format!("for i in $(seq {calls}); do dibs --status > /dev/null; done"),
    );
    say(&format!(
        "  {:>6.1} ms  dibs --status, a call of its own each time",
        fresh * 1000.0 / calls as f64
    ));
    let ticks = |n: usize| {
        children_cpu(
            &s,
            &format!("dibs --watch 1 --json | head -n {n} > /dev/null"),
        )
    };
    let (few, many) = (ticks(2), ticks(12));
    say(&format!(
        "  {:>6.1} ms  a --watch --json tick, past the first",
        (many - few) * 1000.0 / 10.0
    ));
    gate.open();
    s.wait(holder);
}
