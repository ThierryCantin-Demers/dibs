//! A machine whose benchmarks hold only their cards (`benchmarks = "card"`), with real PCI
//! devices of this computer standing in for its cards.

use crate::harness::*;
use std::fs;

/// Two PCI devices here, as `(slot, vendor:device)`, whose bus, device and function no other
/// device shares in another domain: rocminfo tells cards apart by those alone.
fn two_slots() -> Option<[(String, String); 2]> {
    let found: Vec<(String, String)> = fs::read_dir("/sys/bus/pci/devices")
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
        })
        .collect();
    let slot = |pci: &str| pci.split_once(':').map(|(_, s)| s.to_string());
    let mut alone = found
        .iter()
        .filter(|(pci, _)| found.iter().filter(|(o, _)| slot(o) == slot(pci)).count() == 1)
        .cloned();
    Some([alone.next()?, alone.next()?])
}

/// A sandbox on a card machine with `gpu:a` and `gpu:b`; None where there are no slots to use.
fn card_machine() -> Option<Sandbox> {
    machine_of(["gpu:a", "gpu:b"])
}

/// A sandbox on a card machine with two cards under `aliases`.
fn machine_of(aliases: [&str; 2]) -> Option<Sandbox> {
    let Some([(a, chip_a), (b, chip_b)]) = two_slots() else {
        skip("this computer has no two PCI devices to stand in for cards");
        return None;
    };
    let mut s = Sandbox::new();
    let bdf = |pci: &str| {
        let mut parts = pci
            .rsplit([':', '.'])
            .map(|f| u32::from_str_radix(f, 16).unwrap());
        let (function, device, bus) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        (bus << 8) | (device << 3) | function
    };
    let agent = |n: u32, pci: &str| {
        format!(
            "*******\nAgent {n}\n*******\n  Name:                    gfx\n  Uuid:                    GPU-XX\n  Device Type:             GPU\n  BDFID:                   {}\n",
            bdf(pci)
        )
    };
    s.write_exec(
        "bin/rocminfo",
        &format!(
            "#!/bin/sh\ncat <<'EOF'\nHSA Agents\n==========\n{}{}EOF\n",
            agent(1, &a),
            agent(2, &b)
        ),
    );
    let card = |alias: &str, pci: &str, chip: &str| {
        format!(
            "\n  [[machine.here.device]]\n  kind     = \"gpu\"\n  alias    = \"{alias}\"\n  name     = \"a card\"\n  pci      = \"{pci}\"\n  chip     = \"{chip}\"\n  runtimes = [\"rocm\"]\n"
        )
    };
    s.machines(&format!(
        "[machine.here]\nssh      = \"here\"\nhostname = \"{}\"\n{}{}",
        hostname(),
        card(aliases[0], &a, &chip_a),
        card(aliases[1], &b, &chip_b),
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

#[test]
fn a_card_benchmark_queued_before_a_whole_machine_one_goes_first() {
    let Some(mut s) = card_machine() else { return };
    let first = s.gate("first");
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
    let machine = s.spawn(bench(&s, None, "whole", &format!("echo whole >> {order}")));
    s.queued(2);
    first.open();
    assert_eq!(s.wait(a1), 0);
    assert_eq!(s.wait(a2), 0);
    assert_eq!(s.wait(machine), 0);
    assert_eq!(s.read("order"), "a1\na2\nwhole\n");
    s.gone();
}

#[test]
fn a_card_benchmarks_tether_holds_its_card_until_the_job_is_gone() {
    let Some(mut s) = card_machine() else { return };
    let holders = s.p("holders");
    let a = s.spawn(bench(
        &s,
        Some("gpu:a"),
        "a",
        &format!(
            "for p in /proc/[0-9]*; do ls -l $p/fd 2>/dev/null | grep -q /cards/ && tr '\\0' ' ' < $p/cmdline && echo; done > {holders}; true"
        ),
    ));
    assert_eq!(s.wait(a), 0);
    assert!(
        s.read("holders").contains(" tether"),
        "so a runner that dies frees no card while its job winds down: {}",
        s.read("holders")
    );
    s.gone();
}

#[test]
fn an_urgent_benchmark_that_joined_keeps_a_second_one_out() {
    let Some(mut s) = card_machine() else { return };
    let (first, second) = (s.gate("first"), s.gate("second"));
    let order = s.p("order");
    let a = s.spawn(bench(
        &s,
        Some("gpu:a"),
        "a",
        &format!("echo a >> {order}; {}", first.hold()),
    ));
    s.until_records("the first benchmark to run", || s.read("order") == "a\n");
    let urgent = |s: &Sandbox, card: &str, command: &str| {
        s.dibs([
            "--on",
            "here",
            "--bench",
            "--urgent",
            "a release",
            "--wait",
            "0",
            "--device",
            card,
            "--label",
            "u",
            command,
        ])
    };
    let joined = s.spawn(urgent(
        &s,
        "gpu:b",
        &format!("echo b >> {order}; {}", second.hold()),
    ));
    s.until_records("the urgent one to join", || s.read("order") == "a\nb\n");
    let out = urgent(&s, "gpu:a", &format!("echo late >> {order}")).run();
    assert_ne!(out.code, 0, "{}", out.all());
    assert!(
        out.all().contains("One urgent job at a time"),
        "{}",
        out.all()
    );
    first.open();
    second.open();
    assert_eq!(s.wait(a), 0);
    assert_eq!(s.wait(joined), 0);
    assert_eq!(s.read("order"), "a\nb\n");
    s.gone();
}

#[test]
fn an_urgent_benchmark_on_a_free_card_joins_the_ones_running() {
    let Some(mut s) = card_machine() else { return };
    let first = s.gate("first");
    let order = s.p("order");
    let a = s.spawn(bench(
        &s,
        Some("gpu:a"),
        "a",
        &format!("echo a >> {order}; {}", first.hold()),
    ));
    s.until_records("the first benchmark to run", || s.read("order") == "a\n");
    let urgent = s.spawn(s.dibs([
        "--on",
        "here",
        "--bench",
        "--urgent",
        "a release",
        "--wait",
        "0",
        "--device",
        "gpu:b",
        "--label",
        "b",
        &format!("echo b >> {order}"),
    ]));
    assert_eq!(s.wait(urgent), 0, "its card was free");
    assert_eq!(s.read("order"), "a\nb\n");
    first.open();
    assert_eq!(s.wait(a), 0);
    s.gone();
}

/// The cards a benchmark naming `device` ran on, as its job saw them.
fn ran_on(s: &mut Sandbox, device: &str, label: &str) -> (i32, String) {
    let seen = s.p(&format!("seen-{label}"));
    let out = s
        .dibs([
            "--on",
            "here",
            "--bench",
            "--wait",
            "0",
            "--device",
            device,
            "--label",
            label,
            &format!("echo $DIBS_DEVICE > {seen}"),
        ])
        .run();
    (
        out.code,
        s.read(&format!("seen-{label}")).trim().to_string(),
    )
}

#[test]
fn a_prefix_takes_a_free_card_among_the_ones_it_names() {
    let Some(mut s) = machine_of(["gpu:twin.a", "gpu:twin.b"]) else {
        return;
    };
    let first = s.gate("first");
    let order = s.p("order");
    let a = s.spawn(bench(
        &s,
        Some("gpu:twin.a"),
        "a",
        &format!("echo a >> {order}; {}", first.hold()),
    ));
    s.until_records("the first benchmark to run", || s.read("order") == "a\n");
    assert_eq!(
        ran_on(&mut s, "twin", "free"),
        (0, "gpu:twin.b".to_string()),
        "gpu: may be left out, and the card in use is passed over"
    );
    first.open();
    assert_eq!(s.wait(a), 0);
    let (code, both) = ran_on(&mut s, "gpu:twin,twin", "both");
    let mut both: Vec<&str> = both.split(',').collect();
    both.sort_unstable();
    assert_eq!(
        (code, both),
        (0, vec!["gpu:twin.a", "gpu:twin.b"]),
        "each entry takes a card of its own"
    );
    s.gone();
}

#[test]
fn a_label_measured_on_one_card_waits_for_it_rather_than_take_another() {
    let Some(mut s) = machine_of(["gpu:twin.a", "gpu:twin.b"]) else {
        return;
    };
    let (code, card) = ran_on(&mut s, "twin", "series");
    assert_eq!(code, 0);
    let held = s.gate("held");
    let order = s.p("order");
    let busy = s.spawn(bench(
        &s,
        Some(card.as_str()),
        "busy",
        &format!("echo busy >> {order}; {}", held.hold()),
    ));
    s.until_records("its card to be taken", || s.read("order") == "busy\n");
    assert_eq!(
        ran_on(&mut s, "twin", "series").0,
        75,
        "its series is on {card}, so the other card would start a second history"
    );
    held.open();
    assert_eq!(s.wait(busy), 0);
    assert_eq!(ran_on(&mut s, "twin", "series"), (0, card));
    s.gone();
}
