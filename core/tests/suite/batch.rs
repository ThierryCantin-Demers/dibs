use crate::harness::*;

fn batch_file(s: &Sandbox, name: &str, lines: &[&str]) -> String {
    s.write(name, &format!("{}\n", lines.join("\n")));
    s.p(name)
}

fn kill9(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

#[test]
fn a_batch_runs_its_steps_in_order_and_prints_one_summary() {
    let s = Sandbox::new();
    let order = s.p("order");
    let file = batch_file(
        &s,
        "b1",
        &[
            "# a comment",
            &format!("[a] dibs --label batch-a 'echo a-start >> {order}; echo a-end >> {order}'"),
            "",
            &format!("[b] dibs --label batch-b 'echo b >> {order}'"),
        ],
    );
    let out = s.dibs(["batch", &file]).run();
    assert_eq!(s.read("order"), "a-start\na-end\nb\n", "a batch runs its steps in order");
    assert_eq!(out.code, 0, "and exits 0 when every step did");
    assert_eq!(out.stdout.lines().next().unwrap_or("").lines_matching("^batch [0-9-]*  2 steps, "), 1, "its summary is the one thing on stdout");
    assert_eq!(out.stdout.lines_matching(r"^a  .* [0-9]{14}-[0-9]+$"), 1, "naming each step's job");
    assert_eq!(out.stderr.lines_with("nothing to watch"), 1, "and it says when it starts that there is nothing to watch");
    let dir = capture(&out.stdout, r"^each step.s output: (.*)/<name>").unwrap();
    assert_eq!(std::fs::read_to_string(format!("{dir}/a.err")).unwrap().lines_matching("^job "), 1, "each step's output is kept on this side");
    assert!(dir.starts_with(&s.var("HOME")), "under a home of its own here, not the real one: {dir}");
}

#[test]
fn a_failed_step_stops_the_batch_unless_it_says_cont() {
    let s = Sandbox::new();
    let file = batch_file(&s, "b2", &["[x] dibs --label batch-x 'exit 3'", &format!("[y] dibs --label batch-y 'echo ran > {}'", s.p("by"))]);
    let out = s.dibs(["batch", &file]).run();
    assert!(!s.exists("by"), "a failed step stops the batch");
    assert_eq!(out.code, 1, "which exits 1");
    assert_eq!(out.stdout.lines_matching("^x .* 3  |^y .*not run"), 2, "and the summary says which step failed and which did not run");
    assert_eq!(s.dibs(["batch", "--dry-run", &file]).run().code, 0);
    assert!(!s.exists("by"), "a dry run runs nothing");
    let file = batch_file(&s, "b3", &["[x cont] dibs --label batch-x 'exit 3'", &format!("[y] dibs --label batch-y 'echo ran > {}'", s.p("by2"))]);
    let out = s.dibs(["batch", &file]).run();
    assert_eq!(s.read("by2"), "ran\n", "a cont step's failure lets the rest run");
    assert_eq!(out.code, 1, "and the batch still exits 1");
}

#[test]
fn a_batch_reads_stdin_and_refuses_what_is_not_a_dibs_call() {
    let s = Sandbox::new();
    let out = s.dibs(["batch", "-"]).stdin("dibs --label batch-stdin 'echo from-stdin'\n").run();
    assert_eq!(out.stdout.lines_matching("^1  "), 1, "a batch reads stdin");
    let file = batch_file(&s, "b4", &[&format!("dibs --label ok 'echo should-not-run > {}'", s.p("bz")), "cargo build"]);
    assert_eq!(s.dibs(["batch", &file]).code(), 2, "a line that is not a dibs call is refused");
    assert!(!s.exists("bz"), "before anything runs");
}

#[test]
fn a_benchmark_step_naming_no_machine_refuses_the_batch_up_front() {
    // Refused only when its turn came, the steps ahead of it would already have run.
    let s = Sandbox::new();
    s.write("two-machines.toml", "[machine.a]\nssh = \"a\"\nhostname = \"a\"\n\n[machine.b]\nssh = \"b\"\nhostname = \"b\"\n");
    let file = batch_file(
        &s,
        "b-nameless",
        &[&format!("[b1] dibs --label before 'echo ran > {}'", s.p("bnm-ran")), "[b2] dibs --bench --label nameless true"],
    );
    let out = s.dibs(["batch", &file]).env("DIBS_LOCAL", "0").env("DIBS_MACHINES", s.p("two-machines.toml")).run();
    assert_eq!(out.code, 2, "a batch with a benchmark step that names no machine is refused");
    assert!(!s.exists("bnm-ran"), "before any step runs");
    assert_eq!(out.all().lines_with("step b2 measures and names no machine"), 1, "naming the step");
}

#[test]
fn a_killed_driver_takes_its_running_step_and_its_lock_with_it() {
    let mut s = Sandbox::new();
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let file = batch_file(&s, "b5", &[&format!("[hold] dibs --label batch-hold '{}; {}'", up.signal(), hold.hold())]);
    let driver = s.spawn(s.dibs(["batch", &file]));
    up.reached();
    kill9(driver.pid);
    s.wait(driver);
    s.gone();
}

#[test]
fn status_carries_the_batchs_plan_to_the_machine() {
    // The machine sees one step at a time, so the batch's plan travels with each one.
    let mut s = Sandbox::new();
    s.history(&"shared\tbatch-cur\t120\tx\nshared\tbatch-next\t300\tx\n".repeat(3));
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let file = batch_file(
        &s,
        "b6",
        &[
            &format!("[hold] dibs --label batch-cur '{}; {}'", up.signal(), hold.hold()),
            "[next] dibs --label batch-next true",
            "[fresh] dibs --label batch-never-run true",
            "[far] dibs --on elsewhere --label batch-far true",
        ],
    );
    let driver = s.spawn(s.dibs(["batch", &file]));
    up.reached();
    let status = s.dibs(["status"]).run().stdout;
    assert_eq!(status.lines_matching(r"^    batch [0-9]{8}-[0-9]{6}-[0-9]+, step 1 of 4: hold$"), 1, "status names the batch and the step");
    assert_eq!(
        status.lines_matching(r"^    then here: next ~5m00s, fresh \(no history\)$"),
        1,
        "what is still to come on this machine, with its estimate"
    );
    assert_eq!(status.lines_matching("^    then on other machines: far$"), 1, "and what goes elsewhere");
    // The step ahead has been running for a moment, which the time left counts down by.
    assert_eq!(
        status.lines_matching(r"^    batch time left here: over (6m5[5-9]s|7m00s), since some of what is ahead has no history$"),
        1,
        "with the time left for the batch here, as a floor when a step has no history:\n{status}"
    );
    let json = s.dibs(["status", "--json"]).run().stdout;
    assert_eq!(
        json.lines_matching(
            r#""batch":\{"id":"[0-9-]+","step":"hold","k":1,"n":4,"here":2,"elsewhere":1,"next":"next ~5m00s, fresh \(no history\)","far":"far","left":(41[5-9]|420),"left_partial":true\}"#
        ),
        1,
        "the same in json"
    );
    s.history(&"bench\tbatch-queued-bench\t200\tx\n".repeat(3));
    let bench = s.spawn(s.dibs(["--bench", "--label", "batch-queued-bench", "true"]));
    s.queued(1);
    assert_eq!(
        s.dibs(["status"]).run().stdout.lines_matching(r"^    batch time left here: over 10m(1[5-9]|20)s, since some of what is ahead has no history$"),
        1,
        "a benchmark queued now goes ahead of the batch's next step, and the time left counts its wait"
    );
    kill9(driver.pid);
    s.wait(driver);
    s.wait(bench);
    s.gone();
    let events: Vec<Vec<String>> = s
        .log()
        .lines()
        .map(|l| l.split('\t').map(str::to_string).collect::<Vec<_>>())
        .filter(|f| f.get(4).map(String::as_str) == Some("batch-cur"))
        .collect();
    let tagged = regex::Regex::new("^[0-9-]+ hold$").unwrap();
    assert!(
        events.len() >= 2 && events.iter().all(|f| f.get(10).is_some_and(|b| tagged.is_match(b))),
        "the log names the batch and step of every event: {events:?}"
    );
    assert!(s.dibs(["--log", "50"]).run().stdout.lines_matching(r"batch-cur .*\[batch [0-9-]+ hold\]$") >= 1, "and --log shows it");
    assert_eq!(s.count("batch"), 0, "and a record left behind by a killed job goes with it");
}

fn cancellable(s: &mut Sandbox, name: &str) -> (Job, String, String) {
    let (up, hold) = (s.gate(&format!("{name}-up")), s.gate(&format!("{name}-hold")));
    let after = s.p(&format!("{name}-after"));
    let file = batch_file(
        s,
        name,
        &[
            &format!("[hold cont] dibs --label batch-kill-hold '{}; {}'", up.signal(), hold.hold()),
            &format!("[after] dibs --label batch-kill-after 'echo ran > {after}'"),
        ],
    );
    let (out, err) = (s.path(&format!("{name}.out")), s.path(&format!("{name}.err")));
    let call = s.dibs(["batch", &file]).streams_to(&out, &err);
    let driver = s.spawn(call);
    up.reached();
    let id = capture(&s.read(&format!("{name}.err")), r"^dibs: batch ([0-9-]*), ").unwrap();
    (driver, id, format!("{name}-after"))
}

#[test]
fn killing_a_batch_where_its_driver_runs_cancels_all_of_it() {
    let mut s = Sandbox::new();
    let (driver, id, after) = cancellable(&mut s, "b8");
    let out = s.dibs(["--kill", &id]).run().all();
    assert_eq!(s.wait(driver), 76, "killing a batch where its driver runs cancels all of it");
    assert!(!s.exists(&after), "a cont step included");
    assert_eq!(s.read("b8.out").lines().next().unwrap_or("").lines_with(", cancelled with dibs --kill"), 1, "its summary says it was cancelled");
    assert_eq!(out.lines_matching("^batch .*, cancelled with dibs --kill"), 1, "and the kill prints that summary");
    assert_eq!(s.dibs(["--bench", "--wait", "10", "--label", "batch-kill-next", "true"]).code(), 0, "and its running step's lock is released");
}

#[test]
fn killing_a_batch_on_a_machine_stops_its_jobs_and_refuses_its_later_steps() {
    let mut s = Sandbox::new();
    let (driver, id, after) = cancellable(&mut s, "b9");
    let stranger = s.dibs(["--kill", &id]).env_remove("CLAUDE_CODE_HOST_SESSION_ID").env("CLAUDE_CODE_SESSION_ID", "someone-else").env("DIBS_KILL_HERE", "1");
    assert_eq!(
        (stranger.code(), s.count("cancelled")),
        (2, 0),
        "on a machine, another session's batch is not stopped without --anyone"
    );
    let out = s.dibs(["--kill", &id]).env("DIBS_KILL_HERE", "1").run().all();
    assert_eq!(s.wait(driver), 76, "on a machine, it stops the batch's job there, and the driver elsewhere stops with it");
    assert!(!s.exists(&after));
    assert_eq!(out.lines_matching(&format!("^Cancelled batch {id} on .*: stopped 1 job")), 1, "the machine says what it stopped");
    assert_eq!(
        s.read(&format!("home/.local/state/dibs/batch/{id}/hold.err")).lines_with("  exit 76  by=dibs"),
        1,
        "the stopped step's trailer puts its exit on dibs"
    );
    let late = s.dibs(["--label", "batch-kill-late", &format!("echo ran > {}", s.p("late"))]).env("DIBS_BATCH", &id).env("DIBS_BATCH_STEP", "late");
    assert_eq!(late.code(), 76, "and a later step of that batch is refused there");
    assert!(!s.exists("late"));
    assert_eq!(s.dibs(["--bench", "--wait", "10", "--label", "batch-kill-next", "true"]).code(), 0, "and its lock is released");
}
