use crate::harness::*;
use std::fs;

/// A call that leaves this computer, to a machine no test can reach.
fn away(call: Call) -> Call {
    call.env("DIBS_LOCAL", "0").env("DIBS_CONNECT_TIMEOUT", "2")
}

#[test]
fn check_reports_what_nothing_works_without() {
    let s = Sandbox::new();
    let check = s.dibs(["--check"]).run().stdout;
    assert_eq!(check.lines_with("flock and timeout"), 1, "--check reports the tools nothing works without");
    assert_eq!(check.lines_with("parsed the bootstrap"), 1, "it proves the bootstrap parsed by having run at all");
    assert_eq!(check.lines_with("    cpu   "), 1, "it names the cpu");
    // rocm-smi is installed on machines with no AMD GPU, prints a driver error, and exits 0:
    // neither the tool nor its status says anything about the hardware.
    let path = format!("/nonexistent:{}", s.var("PATH"));
    assert_eq!(
        s.dibs(["--check"]).env("PATH", path).run().stdout.lines_matching(r"^  ready\.$"),
        0,
        "a machine with nothing to run on is not called ready"
    );
    assert_eq!(s.dibs(["--check", "a", "b"]).run().stderr.lines_with("only a host"), 1, "--check takes only a host");
}

#[test]
fn an_unreachable_machine_fails_fast_and_says_why() {
    let s = Sandbox::new();
    let out = away(s.dibs(["--status"])).env("DIBS_HOSTNAME", "nowhere").env("DIBS_HOST", "nowhere.invalid").run();
    assert_eq!(out.code, 69, "fails fast with exit 69");
    let all = out.all();
    assert_eq!(all.lines_with("Do not retry in a loop"), 1, "and tells the agent not to loop");
    // ssh's own answer, not a guess: a machine reached over the LAN is not a tailnet peer, and
    // calling it off or asleep sends someone to look at a machine that is fine.
    assert_eq!(all.lines_with("does not resolve from here"), 1, "it says what ssh actually complained about");
    assert_eq!(all.to_lowercase().lines_with("not on the tailnet"), 0, "and does not blame the tailnet for a machine that is not on it");
}

#[test]
fn which_says_why_it_has_nothing() {
    let s = Sandbox::new();
    let inventory = s.p("no-such-inventory");
    let out = s.dibs(["--which"]).env("DIBS_MACHINES", &inventory).env("DIBS_LOCAL", "0").run();
    assert_eq!(out.code, 2);
    assert_eq!(
        out.all(),
        format!("dibs: no machine: no --on, no DIBS_ON, no DIBS_HOST, and no inventory at {inventory}.\n")
    );
}

const DESK_AND_LAP: &str = r#"default = "desk"

[machine.desk]
ssh      = "dibs@desk"
hostname = "desk"

  [[machine.desk.device]]
  kind = "gpu"
  name = "a device, not the machine"

  [[machine.desk.device]]
  kind = "cpu"
  name = "a processor"

[machine.lap]
ssh      = "lap"
hostname = "somewhere-else"
measure  = false
"#;

#[test]
fn the_inventory_lists_every_machine_and_no_default() {
    let mut s = Sandbox::new();
    s.machines(DESK_AND_LAP);
    let out = s.dibs(["--machines"]).run();
    assert_eq!(out.stdout.lines().count(), 2, "lists every machine");
    assert_eq!(out.stdout.lines_matching(r"^ \*"), 0, "marks no machine as a default");
    assert_eq!(out.stderr.lines_with("no longer read"), 1, "and says a default line is no longer read");
    assert_eq!(out.stdout.lines_with("no measurements"), 1, "says which one refuses measurements");
    // A device table's own keys must not answer for the machine's, or a card's name becomes the
    // machine's ssh alias.
    let named = away(s.dibs(["--on", "desk", "--status"])).run().all();
    assert_eq!(named.lines_with("cannot reach 'dibs@desk'"), 1, "a device key does not answer for the machine");
    assert_eq!(named.lines_with("Nothing on this call named a machine"), 0, "a caller who named the machine is not lectured about naming one");
}

#[test]
fn among_several_machines_a_measurement_names_its_own() {
    // A forgotten --on landing on a default starts a series nobody meant.
    let mut s = Sandbox::new();
    s.machines(DESK_AND_LAP);
    let out = s.dibs(["--bench", "--label", "nameless", "true"]).env("DIBS_LOCAL", "0").run();
    assert_eq!(out.code, 2, "a benchmark that names no machine is refused");
    let all = out.all();
    assert_eq!(all.lines_with("Name one of: desk, lap"), 1, "and it names the machines");
    assert_eq!(all.lines_with("never placed for you"), 1, "and says a measurement is never placed");
    assert_eq!(all.lines_with("export DIBS_ON"), 1, "and how to cover a whole script at once");
    assert_eq!(s.dibs(["--peek", "true"]).env("DIBS_LOCAL", "0").code(), 2, "a peek that names none is refused too");
    let status = away(s.dibs(["--status"])).env("DIBS_POLL_TIMEOUT", "3").env("DIBS_CONNECT_TIMEOUT", "1").run().all();
    assert_eq!(status.lines_matching("^(desk|lap)$"), 2, "a status that names none shows every machine");
    let out = s.dibs(["--bench", "true"]).env("DIBS_HOST", "pinned.invalid").env("DIBS_LOCAL", "0").run().all();
    assert_eq!(out.lines_with("DIBS_HOST=pinned.invalid does not choose"), 1, "DIBS_HOST does not choose among several");
}

#[test]
fn a_cpu_can_be_named_as_a_device() {
    // A CPU has no PCI address to be found by, and refusing it left a CPU benchmark unable to name
    // what it ran on while being told it had named none of the GPUs.
    let mut s = Sandbox::new();
    s.machines(DESK_AND_LAP);
    let out = away(s.dibs(["--on", "desk", "--device", "cpu", "--bench", "true"])).run().all();
    assert_eq!(out.lines_with("no device called"), 0, "a cpu can be named as a device");
    assert_eq!(out.lines_with("named none of them"), 0, "and naming it silences the unpinned-GPU notice");
    let out = s.dibs(["--on", "desk", "--device", "nope", "--bench", "true"]).run().all();
    assert_eq!(out.lines().filter(|l| *l == "    cpu").count(), 1, "a name that is neither is still refused, and offered the cpu");
}

/// A PCI device this computer has, by address and chip, standing in for a card.
fn any_pci_device() -> Option<(String, String)> {
    let dev = fs::read_dir("/sys/bus/pci/devices").ok()?.flatten().next()?.path();
    let read = |f: &str| fs::read_to_string(dev.join(f)).ok().map(|v| v.trim().trim_start_matches("0x").to_string());
    Some((dev.file_name()?.to_string_lossy().into_owned(), format!("{}:{}", read("vendor")?, read("device")?)))
}

#[test]
fn a_card_no_longer_in_its_slot_is_refused_rather_than_run_on_another() {
    // Vulkan's selectors ignore an address that answers to nothing and hand the job the first card.
    let Some((pci, chip)) = any_pci_device() else { return };
    let mut s = Sandbox::new();
    let card = |alias: &str, pci: &str, chip: &str| {
        format!("\n  [[machine.here.device]]\n  kind     = \"gpu\"\n  alias    = \"{alias}\"\n  name     = \"a card\"\n  pci      = \"{pci}\"\n  chip     = \"{chip}\"\n  runtimes = [\"vulkan\"]\n")
    };
    s.machines(&format!(
        "[machine.here]\nssh      = \"here\"\nhostname = \"{}\"\n{}{}{}",
        hostname(),
        card("gpu:there", &pci, &chip),
        card("gpu:swapped", &pci, "ffff:0000"),
        card("gpu:gone", "0000:ff:1f.7", &chip)
    ));
    let run = |alias: &str| s.dibs(["--on", "here", "--device", alias, "echo ran on $DIBS_DEVICE"]).run();
    let out = run("gpu:there");
    assert_eq!((out.code, out.stdout.lines_matching("^ran on gpu:there$")), (0, 1), "the card in its slot runs: {}", out.all());
    let out = run("gpu:swapped");
    assert_eq!(
        (out.code, out.stdout.lines_with("ran on"), out.stderr.lines_with(&format!("recorded as ffff:0000 in {pci}, and that slot now holds {chip}"))),
        (2, 0, 1),
        "a slot holding another model is refused: {}",
        out.all()
    );
    let out = run("gpu:gone");
    assert_eq!((out.code, out.stdout.lines_with("ran on"), out.stderr.lines_with("that slot now holds nothing")), (2, 0, 1), "and so is an empty one: {}", out.all());
}

#[test]
fn an_unknown_machine_is_refused_and_the_known_ones_named() {
    let mut s = Sandbox::new();
    s.machines(DESK_AND_LAP);
    let out = s.dibs(["--on", "nope", "--status"]).run();
    assert_eq!(out.code, 2, "an unknown machine is refused");
    assert_eq!(out.all().lines_with("desk"), 1, "and the known ones are named");
}

#[test]
fn a_machine_that_does_not_measure_refuses_a_benchmark() {
    // A machine that cannot produce a trustworthy number must not be able to produce one at all,
    // rather than everyone remembering not to ask it.
    let mut s = Sandbox::new();
    s.machines(DESK_AND_LAP);
    let out = s.dibs(["--on", "lap", "--bench", "true"]).run();
    assert_eq!(out.code, 2, "a benchmark is refused where measure is false");
    assert_eq!(out.all().lines_with("measure = false"), 1, "and it says why");
    s.machines("[machine.lap]\nssh = \"me@lap\"\nhostname = \"lap\"\nmeasure = false\n");
    let out = s.dibs(["--bench", "true"]).env("DIBS_LOCAL", "0").env("DIBS_HOST", "me@lap").run();
    assert_eq!(out.code, 2, "a benchmark sent by ssh string to a machine that does not measure is refused");
    assert_eq!(out.all().lines_with("measure = false"), 1, "and says so");
}

#[test]
fn recording_a_machine_writes_its_entry_and_nothing_else() {
    let mut s = Sandbox::new();
    s.machines("[machine.desk]\nssh      = \"dibs@desk\"\nhostname = \"desk\"\n");
    // An integrated GPU has no link of its own and reports width 0 against a max of 255, which
    // once divided by zero and took the whole entry with it.
    let out = s.dibs(["--check", "laptop", "--write"]).run().all();
    assert_eq!(out.lines_with("reported nothing to record"), 0, "a device with no pcie link of its own does not break the write");
    // --check once dialled its argument literally, so the command whose job is to say whether a
    // machine is usable was the one that could not reach it.
    assert_eq!(
        away(s.dibs(["--check", "desk"])).run().all().lines_with("cannot reach 'dibs@desk'"),
        1,
        "a known name is resolved, not dialled literally"
    );
    assert_eq!(
        away(s.dibs(["--check", "brand-new-host"])).run().all().lines_with("cannot reach 'brand-new-host'"),
        1,
        "and a name nobody has recorded is still taken literally, so it can onboard one"
    );
    assert_eq!(s.read("machines.toml").lines_matching("^default"), 0, "recording a machine writes no default");
    assert_eq!(s.dibs(["--machines"]).run().stdout.lines().count(), 2, "and the new machine is there too");
}

#[test]
fn forgetting_a_machine_takes_its_devices_with_it() {
    // Renaming a machine leaves an entry that can never answer, and the only fix was editing the
    // file by hand.
    let mut s = Sandbox::new();
    s.machines(
        r#"[machine.old]
ssh      = "old"
hostname = "old"
measure  = false

  [[machine.old.device]]
  kind = "gpu"
  name = "a device whose parent is going away"

[machine.new]
ssh      = "new"
hostname = "new"
"#,
    );
    assert_eq!(s.dibs(["--forget", "old"]).code(), 0, "forgetting a machine succeeds");
    assert_eq!(s.dibs(["--machines"]).run().stdout.lines().count(), 1, "and it is gone");
    assert_eq!(s.read("machines.toml").lines_with("parent is going away"), 0, "its device table goes with it");
    assert_eq!(
        s.dibs(["--which"]).env("DIBS_LOCAL", "0").run().stdout.trim_end(),
        "new",
        "the only machine is used without naming it"
    );
    assert_eq!(s.dibs(["--forget", "nope"]).code(), 2, "forgetting one that is not there is refused");
}

#[test]
fn a_shared_registry_sits_under_your_own_machines() {
    let mut s = Sandbox::new();
    s.write(
        "registry.toml",
        "[machine.team-box]\nssh      = \"dibs@team-box\"\nhostname = \"team-box\"\n\n\
         [machine.contested]\nssh      = \"dibs@from-registry\"\nhostname = \"from-registry\"\n",
    );
    s.set("DIBS_REGISTRY_CACHE", s.p("registry.toml"));
    s.machines(
        "[machine.mine]\nssh      = \"dibs@mine\"\nhostname = \"mine\"\n\n\
         [machine.contested]\nssh      = \"dibs@from-mine\"\nhostname = \"from-mine\"\n",
    );
    let listed = s.dibs(["--machines"]).run().stdout;
    assert_eq!(listed.lines().count(), 3, "both layers are listed");
    assert_eq!(listed.lines_with("team-box"), 1, "a shared machine is usable without writing it out");
    assert_eq!(listed.lines_with("[shared]"), 1, "and it says which layer it came from");
    // The one only you have, and the shared one you overrode, which is yours now.
    assert_eq!(listed.lines_with("[yours]"), 2, "your own machines are marked as yours");
    // Half an entry from each file would describe a machine that exists nowhere.
    assert_eq!(
        away(s.dibs(["--on", "contested", "--status"])).run().all().lines_with("dibs@from-mine"),
        1,
        "a personal entry overrides the shared one whole"
    );
    let out = s.dibs(["--forget", "team-box"]).run();
    assert_eq!(out.code, 2, "a shared machine cannot be forgotten locally");
    assert_eq!(out.all().lines_with("shared registry"), 1, "and it says why");
    assert_eq!(s.dibs(["--forget", "mine"]).code(), 0, "your own can");
}

fn one_and_two(s: &mut Sandbox) {
    s.machines(&format!(
        "[machine.one]\nssh      = \"one\"\nhostname = \"{}\"\nworkstation = true\n\n[machine.two]\nssh      = \"two\"\nhostname = \"two\"\n",
        hostname()
    ));
}

fn pick(call: Call) -> String {
    call.run().stdout.trim_end().to_string()
}

#[test]
fn routing_ranks_the_machines_that_answered() {
    let mut s = Sandbox::new();
    one_and_two(&mut s);
    assert_eq!(s.dibs(["--pick"]).run().stdout.lines_matching("^(one|two)$"), 1, "picks a machine from the inventory");
    // A machine that did not answer costs the whole probe timeout on every dispatch until it is
    // back, so it is left out of the ranking for a while, and said so.
    s.write("down/two", &now().to_string());
    let down = s.p("down");
    let verbose = |s: &Sandbox| s.dibs(["--pick", "-v"]).env("DIBS_ROUTE_DOWN", &down).run().stderr;
    assert_eq!(verbose(&s).lines_with("not asked again yet"), 1, "a machine that did not answer is not asked again yet");
    assert_eq!(pick(s.dibs(["--pick"]).env("DIBS_ROUTE_DOWN", &down)), "one", "and the ranking goes on without it");
    s.write("down/two", "0");
    assert_eq!(verbose(&s).lines_with("not asked again yet"), 0, "until the backoff has passed");
}

#[test]
fn routing_prefers_the_cache_over_an_idle_machine() {
    let mut s = Sandbox::new();
    one_and_two(&mut s);
    let penalised = |args: &[&str]| s.dibs(args).env("DIBS_SELF_PENALTY", "10000");
    // A build takes every thread on the machine its owner is trying to work on.
    assert_eq!(pick(penalised(&["--pick"])), "two", "a machine someone works at is ranked behind an equal one");
    assert_eq!(penalised(&["--pick", "-v"]).run().stderr.lines_with("someone works here"), 1, "-v says why");
    // A build ranked onto one machine and a benchmark pinned to another leaves the benchmark to
    // compile inside its own exclusive lock.
    assert_eq!(pick(penalised(&["--pick", "--prefer", "one"])), "one", "the machine holding the cache wins anyway");
    assert_eq!(penalised(&["--pick", "-v", "--prefer", "one"]).run().stderr.lines_with("holds the cache"), 1, "and it says that is why");
    assert_eq!(pick(penalised(&["--pick", "--prefer", "nowhere"])), "two", "a preferred machine that cannot answer is not used");
}

#[test]
fn which_resolves_names_and_refuses_to_choose() {
    let mut s = Sandbox::new();
    one_and_two(&mut s);
    assert_eq!(pick(s.dibs(["--which"]).env("DIBS_HOST", "dibs@two")), "two", "an ssh string resolves to its inventory entry");
    assert_eq!(s.dibs(["--which"]).env("DIBS_LOCAL", "0").code(), 2, "--which names nothing when several could be meant");
    assert_eq!(pick(s.dibs(["--which"]).env("DIBS_ON", "two")), "two", "DIBS_ON pins to an inventory machine");
}

#[test]
fn a_machine_reports_its_caches_and_clones() {
    let mut s = Sandbox::new();
    one_and_two(&mut s);
    s.write("scr/target/faux/.rustc_info.json", "");
    fs::create_dir_all(s.path("scr/target/prepared-only")).unwrap();
    let json = s.dibs(["--status", "--json"]).env("DIBS_SCRATCH", s.p("scr")).run().stdout;
    assert!(json.contains(r#""caches":["faux"]"#), "a machine reports the repos it has actually built");
    assert!(!json.contains("prepared-only"), "a target directory nothing was built in is not a cache");
    fs::create_dir_all(s.path("fakehome/prog/faux/.git")).unwrap();
    let json = s.dibs(["--status", "--json"]).env("HOME", s.p("fakehome")).run().stdout;
    assert!(json.contains(r#""clones":["faux"]"#), "a machine reports the repos it can prepare from");
}

#[test]
fn a_machine_with_no_clone_of_the_repo_is_not_chosen() {
    // A worktree is prepared from a clone, so a machine without one cannot run the job at all.
    // Ranking it last is not enough: last still wins when it is the only machine that answered.
    let mut s = Sandbox::new();
    one_and_two(&mut s);
    fs::create_dir_all(s.path("fakehome/prog/faux/.git")).unwrap();
    let home = s.p("fakehome");
    let at_home = |args: &[&str]| s.dibs(args).env("HOME", &home);
    let out = at_home(&["--pick", "--repo", "absent"]).env("DIBS_SELF_PENALTY", "0").run();
    assert_eq!((out.code, out.stdout.as_str()), (69, ""), "a machine with no clone of the repo is not chosen");
    assert_eq!(at_home(&["--pick", "--repo", "absent", "-v"]).run().stderr.lines_with("no clone of absent"), 2, "-v says why it was dropped");
    assert_eq!(
        pick(at_home(&["--pick", "--repo", "faux"]).env("DIBS_SELF_PENALTY", "10000")),
        "two",
        "a machine that does have the clone is still chosen"
    );
    // Affinity is a memo about where a cache is, and a cache is no use where the tree cannot be
    // prepared.
    assert_eq!(at_home(&["--pick", "--repo", "absent", "--prefer", "one"]).code(), 69, "and affinity does not override a missing clone");
    // Nothing is up, versus everything is up and none of it can prepare this repo: reporting the
    // second as the first sends you to look at the network.
    assert_eq!(at_home(&["--pick", "--repo", "absent"]).run().stderr.lines_with("none has a clone"), 1, "and says which of the two failures it was");
    assert_eq!(
        at_home(&["--pick", "--repo", "faux"]).env("DIBS_SCRATCH", s.p("scr")).run().stdout.lines_matching("^(one|two)$"),
        1,
        "--repo picks a machine that reports it"
    );
}

#[test]
fn an_uncached_repo_goes_to_a_machine_that_can_measure_it() {
    // A first build decides where a repo's cache lives, and no benchmark can follow it to a
    // machine that refuses benchmarks.
    let mut s = Sandbox::new();
    fs::create_dir_all(s.path("fakehome/prog/never-built/.git")).unwrap();
    s.machines(&format!(
        "[machine.measures]\nssh      = \"measures\"\nhostname = \"{}\"\n\n[machine.refuses]\nssh      = \"refuses\"\nhostname = \"refuses\"\nmeasure  = false\n",
        hostname()
    ));
    let out = s.dibs(["--pick", "--repo", "never-built"]).env("HOME", s.p("fakehome")).env("DIBS_SELF_PENALTY", "100");
    assert_eq!(pick(out), "measures");
}

#[test]
fn shared_work_naming_no_machine_is_placed_on_one_that_answered() {
    let mut s = Sandbox::new();
    s.machines(&format!(
        "[machine.here]\nssh      = \"here\"\nhostname = \"{}\"\n\n[machine.gone]\nssh      = \"nowhere.invalid\"\nhostname = \"gone\"\n",
        hostname()
    ));
    assert_eq!(away(s.dibs(["--label", "r", "true"])).code(), 0, "a shared job that names no machine is placed on the one that answered");
    // A machine dropping out of the ranking silently degrades this to "whichever answered", which
    // looks exactly like a working ranking.
    assert_eq!(
        away(s.dibs(["--pick", "-v"])).run().stderr.lines_matching("gone .*no answer"),
        1,
        "a machine that did not answer says so"
    );
    // Every step of a run lands on the machine its worktree was prepared on, so a step arriving
    // without one is refused rather than placed on its own.
    assert_eq!(
        away(s.dibs(["--label", "r", "true"])).env("DIBS_FROM_RUN", "1").code(),
        2,
        "a step of a run is never placed on its own"
    );
}
