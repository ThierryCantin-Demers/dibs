//! A machine whose benchmarks hold only their cards (`benchmarks = "card"`), with real PCI
//! devices of this computer standing in for its cards.

use crate::harness::*;
use std::fs;

/// Two PCI devices here, as `(slot, vendor:device)`.
fn two_slots() -> Option<[(String, String); 2]> {
    let mut found = fs::read_dir("/sys/bus/pci/devices")
        .ok()?
        .flatten()
        .filter_map(|dev| {
            let read = |f: &str| {
                fs::read_to_string(dev.path().join(f))
                    .ok()
                    .map(|v| v.trim().trim_start_matches("0x").to_string())
            };
            Some((
                dev.file_name().to_string_lossy().into_owned(),
                format!("{}:{}", read("vendor")?, read("device")?),
            ))
        });
    Some([found.next()?, found.next()?])
}

/// A sandbox on a card machine with `gpu:a` and `gpu:b`; None where there are no slots to use.
fn card_machine() -> Option<Sandbox> {
    let Some([(a, chip_a), (b, chip_b)]) = two_slots() else {
        skip("this computer has no two PCI devices to stand in for cards");
        return None;
    };
    let mut s = Sandbox::new();
    let card = |alias: &str, pci: &str, chip: &str| {
        format!(
            "\n  [[machine.here.device]]\n  kind     = \"gpu\"\n  alias    = \"{alias}\"\n  name     = \"a card\"\n  pci      = \"{pci}\"\n  chip     = \"{chip}\"\n"
        )
    };
    s.machines(&format!(
        "[machine.here]\nssh      = \"here\"\nhostname = \"{}\"\n{}{}",
        hostname(),
        card("gpu:a", &a, &chip_a),
        card("gpu:b", &b, &chip_b),
    ));
    s.set("DIBS_BENCHMARKS", "card");
    Some(s)
}

/// A benchmark here, on `card` or on the whole machine.
fn bench(s: &Sandbox, card: Option<&str>, label: &str, command: &str) -> Call {
    let mut args = vec!["--on", "here", "--bench", "--label", label];
    if let Some(card) = card {
        args.extend(["--device", card]);
    }
    args.push(command);
    s.dibs(args)
}

#[test]
fn benchmarks_on_two_cards_run_at_once_and_shared_work_waits_for_the_last_of_them() {
    let Some(mut s) = card_machine() else { return };
    let (on_a, on_b) = (s.gate("a"), s.gate("b"));
    let order = s.p("order");
    let a = s.spawn(bench(
        &s,
        Some("gpu:a"),
        "a",
        &format!("echo gpu:a >> {order}; {}", on_a.hold()),
    ));
    s.until_records("the first card's benchmark to run", || {
        s.read("order") == "gpu:a\n"
    });
    let b = s.spawn(bench(
        &s,
        Some("gpu:b"),
        "b",
        &format!("echo gpu:b >> {order}; {}", on_b.hold()),
    ));
    s.until_records("the second card's benchmark to run beside it", || {
        s.read("order") == "gpu:a\ngpu:b\n"
    });
    let shared = s.spawn(s.dibs([
        "--on",
        "here",
        "--label",
        "build",
        &format!("echo build >> {order}"),
    ]));
    s.queued(1);
    on_a.open();
    assert_eq!(s.wait(a), 0);
    assert_eq!(
        s.read("order"),
        "gpu:a\ngpu:b\n",
        "the machine stays held while the benchmark that joined runs"
    );
    on_b.open();
    assert_eq!(s.wait(b), 0);
    assert_eq!(s.wait(shared), 0);
    assert_eq!(s.read("order"), "gpu:a\ngpu:b\nbuild\n");
    s.gone();
}

#[test]
fn a_benchmark_waiting_for_a_busy_card_holds_up_nobody_on_another() {
    let Some(mut s) = card_machine() else { return };
    let (first, second) = (s.gate("first"), s.gate("second"));
    let order = s.p("order");
    let a1 = s.spawn(bench(
        &s,
        Some("gpu:a"),
        "a1",
        &format!("echo a1 >> {order}; {}", first.hold()),
    ));
    s.until_records("the first benchmark to run", || s.read("order") == "a1\n");
    let a2 = s.spawn(bench(
        &s,
        Some("gpu:a"),
        "a2",
        &format!("echo a2 >> {order}"),
    ));
    s.queued(1);
    let b = s.spawn(bench(
        &s,
        Some("gpu:b"),
        "b",
        &format!("echo b >> {order}; {}", second.hold()),
    ));
    s.until_records("the other card's benchmark to go ahead", || {
        s.read("order") == "a1\nb\n"
    });
    first.open();
    assert_eq!(s.wait(a1), 0);
    assert_eq!(s.wait(a2), 0, "the card it waited for came free");
    assert_eq!(s.read("order"), "a1\nb\na2\n");
    second.open();
    assert_eq!(s.wait(b), 0);
    s.gone();
}

#[test]
fn a_benchmark_naming_no_card_closes_the_machine_to_card_benchmarks_after_it() {
    let Some(mut s) = card_machine() else { return };
    let (first, whole) = (s.gate("first"), s.gate("whole"));
    let order = s.p("order");
    let a = s.spawn(bench(
        &s,
        Some("gpu:a"),
        "a",
        &format!("echo a >> {order}; {}", first.hold()),
    ));
    s.until_records("the card benchmark to run", || s.read("order") == "a\n");
    let machine = s.spawn(bench(
        &s,
        None,
        "whole",
        &format!("echo whole >> {order}; {}", whole.hold()),
    ));
    s.gate_taken();
    let b = s.spawn(bench(&s, Some("gpu:b"), "b", &format!("echo b >> {order}")));
    s.queued(2);
    first.open();
    assert_eq!(s.wait(a), 0);
    s.until_records("the whole-machine benchmark to run", || {
        s.read("order") == "a\nwhole\n"
    });
    whole.open();
    assert_eq!(s.wait(machine), 0);
    assert_eq!(s.wait(b), 0);
    assert_eq!(s.read("order"), "a\nwhole\nb\n");
    s.gone();
}
