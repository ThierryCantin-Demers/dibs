use crate::harness::*;
use std::fs;

/// A repo the recipe layer can prepare: its clone on the machine's side, and a tree here.
fn app(s: &Sandbox) -> String {
    s.git(".", &["init", "-q", "--bare", "origin.git"]);
    s.git(".", &["clone", "-q", "origin.git", "app"]);
    s.write("app/a.txt", "x\n");
    s.write("app/.gitignore", "target\n");
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "one"]);
    s.git("app", &["push", "-q", "origin", "HEAD:main"]);
    fs::create_dir_all(s.path("home/prog")).unwrap();
    s.git(".", &["clone", "-q", "origin.git", "home/prog/app"]);
    s.p("app")
}

fn recipes(s: &Sandbox, toml: &str) {
    s.write("app/.dibs.toml", toml);
}

fn arrivals(s: &Sandbox) -> usize {
    s.log().lines_with("\tarrived\t")
}

fn runs(s: &Sandbox) -> String {
    s.read("home/.local/state/dibs/runs.jsonl")
}

fn last_run(s: &Sandbox, label: &str) -> String {
    let needle = format!("\"label\":\"{label}\"");
    runs(s).lines().rfind(|l| l.contains(&needle)).unwrap_or_default().to_string()
}

/// A cargo that builds nothing, so a recipe's build and the checks on it are all that run.
fn fake_cargo(s: &Sandbox) -> String {
    s.write_exec("fc/cargo", "#!/bin/bash\necho \"    Finished release\"\n");
    s.p("fc/cargo")
}

const PARAMS: &str = r#"
[build.p]
  [build.p.params]
  backend = { choices = ["cuda", "vulkan"], default = "cuda" }
  samples = { default = "10" }
  [[build.p.step]]
  lock = "shared"
  env = { SAMPLES = "{samples}" }
  run = "echo ran {backend} samples=$SAMPLES"

[build.need]
  [build.need.params]
  size = {}
  [[build.need.step]]
  lock = "shared"
  run = "echo {size}"

[build.rel]
  [[build.rel.step]]
  lock = "shared"
  run = "ls target/release"

[bench.hot]
  [[bench.hot.step]]
  lock = "exclusive"
  run = "cargo bench --bench gemm"
"#;

fn gate_recipes(cargo: &str) -> String {
    format!(
        r#"
[bench.gate]
  [[bench.gate.step]]
  lock = "shared"
  run = "{cargo} build"
  [[bench.gate.step]]
  lock = "exclusive"
  run = "echo measured"

[bench.stolen]
  [[bench.stolen.step]]
  lock = "shared"
  run = "{cargo} build"
  [[bench.stolen.step]]
  lock = "shared"
  run = "echo /another/tree > \"$CARGO_TARGET_DIR/.dibs-tree\""
  [[bench.stolen.step]]
  lock = "exclusive"
  run = "echo measured"
"#
    )
}

#[test]
fn a_local_recipe_runs_in_the_tree_it_sent() {
    let s = Sandbox::new();
    let app = app(&s);
    let scratch = s.var("DIBS_SCRATCH");
    let n0 = arrivals(&s);
    let out = s.dibs(["shell", &format!("{app}@local"), "--reason", "test", "--", r#"echo "in $PWD"; cat a.txt"#]).run();
    assert_eq!(out.code, 0, "a local recipe runs: {}", out.stderr);
    assert_eq!(
        (out.stdout.lines_matching(&format!("^in {scratch}/ws/app/local-")), out.stdout.lines_matching("^x$")),
        (1, 1),
        "in the tree it sent"
    );
    assert_eq!(arrivals(&s) - n0, 2, "in two jobs, the setup riding with the transfer");
    assert_eq!(out.stderr.lines_matching(&format!("^dibs: {scratch}/ws/app/local-")), 1, "and says where it prepared");
    assert_eq!(out.all().lines_matching("^DIBS-"), 0, "without the setup's report in the output");
}

#[test]
fn a_recipe_at_a_ref_runs_in_one_job() {
    let s = Sandbox::new();
    let app = app(&s);
    let n0 = arrivals(&s);
    let out = s.dibs(["shell", &format!("{app}@main"), "--reason", "test", "--", r#"echo "in $PWD"; cat a.txt"#]).run();
    assert_eq!(out.code, 0, "a recipe at a ref runs: {}", out.stderr);
    assert_eq!(arrivals(&s) - n0, 1, "in one job, the setup riding with its first step");
    assert_eq!(out.stdout.lines_matching(&format!("^in {}/ws/app/", s.var("DIBS_SCRATCH"))), 1, "in its worktree");
    assert_eq!(
        s.log().lines_matching(r#"	arrived	.*	app_shell	.*	# echo "in \$PWD"; cat a.txt "#),
        1,
        "and the log shows the step's command rather than the setup's"
    );
}

#[test]
fn over_ssh_the_setup_rides_with_rsyncs_own_stream() {
    let s = Sandbox::new();
    let app = app(&s);
    s.write("app/b.txt", "y\n");
    let n0 = arrivals(&s);
    let out = s.remote(s.dibs(["shell", &format!("{app}@local"), "--reason", "test", "--", "cat b.txt"])).run();
    assert_eq!((out.code, out.stdout.as_str(), arrivals(&s) - n0), (0, "y\n", 2), "over ssh, the setup rides with rsync's own stream: {}", out.stderr);
    let strays = fs::read_dir(&s.root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("local-")).count();
    assert_eq!(strays, 0, "and the transfer lands in the tree, nowhere else");
    let log = s.log();
    let batches: std::collections::BTreeSet<_> =
        log.lines().rev().take(4).filter_map(|l| l.split('\t').nth(10)).map(|b| b.split(' ').next().unwrap().to_string()).collect();
    assert_eq!(batches.len(), 1, "both its jobs reach the log as steps of one batch: {batches:?}");
    assert!(regex::Regex::new(r"^[0-9]{8}-[0-9]{6}-[0-9]+$").unwrap().is_match(batches.iter().next().unwrap()));
}

#[test]
fn a_recipe_waiting_in_a_batch_is_planned_as_the_jobs_it_will_make() {
    let mut s = Sandbox::new();
    let app = app(&s);
    s.history(&"rsh\tapp_shell_send\t4\tx\nshared\tapp_shell\t60\tx\nshared\tbatch-rhold\t10\tx\n".repeat(3));
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let steps = format!(
        "[hold] dibs --label batch-rhold '{}; {}'\n[rec] dibs shell {app}@local --reason test -- true\n",
        up.signal(),
        hold.hold()
    );
    s.write("b7", &steps);
    let driver = s.spawn(s.dibs(["batch", &s.p("b7")]));
    up.reached();
    let status = s.dibs(["status"]).run().stdout;
    assert_eq!(
        status.lines_matching(r"^    then here: rec: app/shell:send ~4s, rec: app/shell ~[0-9ms]+$"),
        1,
        "a recipe waiting in a batch is planned as the jobs it will make, each estimated:\n{status}"
    );
    assert_eq!(status.lines_matching(r"^    batch time left here: ~[0-9ms]+$"), 1, "so a batch of recipes has a time left");
    assert_eq!(status.lines_matching(" on -$"), 0, "and a job run on no particular card does not claim one called -");
    hold.open();
    s.wait(driver);
}

#[test]
fn a_repo_runs_a_command_here_against_the_servers_it_declares() {
    // A client is a command rather than a launch line, a port and a kill, written out again in
    // every script that needs them.
    let s = Sandbox::new();
    let app = app(&s);
    s.write(
        "app/serve.py",
        "import os, signal, socket\ns = socket.socket()\ns.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\ns.bind((\"0.0.0.0\", int(os.environ[\"DIBS_PORT_API\"])))\ns.listen()\nsignal.pause()\n",
    );
    s.write(
        "wclient.py",
        "import os, socket\nwhere = os.environ[\"DIBS_SERVICE_API\"]\nhost, _, port = where.rpartition(\":\")\nsocket.create_connection((host, int(port)), timeout=10)\nprint(\"the client reached\", where)\n",
    );
    recipes(&s, "[service.servers]\nbuild = \"echo built > built.txt\"\nports = [\"api\"]\n\n[[service.servers.serve]]\nname = \"api\"\nrun = \"python3 serve.py\"\nready = \"tcp:api\"\n");
    assert_eq!(s.dibs(["list", &app]).run().stdout.lines_matching(r"^  servers   \(from the repo\)$"), 1, "a repo says which servers it defines");
    let out = s.dibs(["with", &format!("{app}@local"), "servers", "--", "python3", &s.p("wclient.py")]).run();
    assert_eq!(out.all().lines_matching("^the client reached [^:]+:[0-9]+$"), 1, "and a command here runs against them, on a port neither side named");
    assert_eq!(out.code, 0, "exiting with the command's own status");
    assert_eq!(s.log().lines_with("app_with_servers_build"), 2, "after building them under the shared lock");
    assert_eq!(out.all().lines_matching("with api: ready after [0-9]*s, stopped when the command ended"), 1, "and stopping them when it ends");
    assert_eq!(
        s.dibs(["with", &format!("{app}@local"), "nope", "--", "true"]).run().all().lines_with("It has: servers"),
        1,
        "a service it does not define says what it has"
    );
    assert_eq!(
        s.dibs(["with", &format!("{app}@main..local"), "servers", "--", "true"]).run().all().lines_with("takes one ref"),
        1,
        "and with runs against one tree only"
    );
}

#[test]
fn a_recipe_says_what_values_it_takes_and_refuses_the_rest() {
    // One procedure covers a sweep instead of fifteen near-identical copies of it, and the set of
    // valid invocations stays something dibs can print.
    let s = Sandbox::new();
    let app = app(&s);
    recipes(&s, PARAMS);
    let local = format!("{app}@local");
    let listed = s.dibs(["list", &app]).run().stdout;
    assert_eq!(listed.lines_matching("^      --samples 10$"), 1, "a recipe says what it takes, with the default");
    assert_eq!(listed.lines_matching("^      --backend cuda  one of cuda, vulkan$"), 1, "and what the choices are where there are any");
    assert_eq!(listed.lines_matching("^      --size <value>, required$"), 1, "one with no default says it is required");
    let build = |extra: &[&str]| {
        let mut args = vec!["build", local.as_str()];
        args.extend_from_slice(extra);
        s.dibs(args).run()
    };
    assert_eq!(build(&["p"]).stdout.lines_matching("^ran cuda samples=10$"), 1, "a parameter left out takes its default, in the command and in what is exported");
    assert_eq!(build(&["p", "--backend", "vulkan", "--samples", "30"]).stdout.lines_matching("^ran vulkan samples=30$"), 1, "and given, it reaches both");
    assert_eq!(runs(&s).lines_with(r#""params":{"backend":"vulkan","samples":"30"}"#), 1, "the run record carries what it was set to");
    assert_eq!(runs(&s).lines_with(r#""label":"app/build/p""#), 2, "but the label does not, so one recipe keeps one history");
    let out = build(&["p", "--backend", "metal"]);
    assert_eq!(out.code, 2, "a value outside the choices is refused before anything is sent");
    assert_eq!(out.all().lines_with("not one of: cuda, vulkan"), 1, "saying which are allowed");
    let out = build(&["p", "--backends", "cuda"]);
    assert_eq!(out.code, 2, "a name the recipe does not declare is refused");
    assert_eq!(out.all().lines_with("this recipe takes: backend, samples"), 1, "saying which names it takes");
    assert_eq!(build(&["need"]).all().lines_with("--size has no default"), 1, "and one with no default cannot be left out");
    let out = build(&["rel"]);
    assert_eq!(out.code, 2, "a step naming a relative target/ is refused: the build writes elsewhere");
    assert_eq!(out.all().lines_with("Use $CARGO_TARGET_DIR/... instead."), 1, "and is told where the build actually writes");
    let dry = build(&["p", "--samples", "30", "--dry-run"]).stdout;
    assert_eq!(dry.lines_matching("^param       samples = 30$"), 1, "a dry run says what the parameters came out as");
    assert_eq!(dry.lines_matching("^            env SAMPLES=30$"), 1, "and what each step will export");
    let out = s.dibs(["bench", &local, "hot"]).run();
    assert_eq!(out.code, 2, "a measured step that would compile is refused");
    assert_eq!(out.all().lines_with(r#"run = "cargo bench --bench gemm --no-run""#), 1, "with the two-step form to replace it");
}

#[test]
fn a_benchmark_recipe_that_names_no_machine_is_refused() {
    // A measurement is never placed: with several machines and none named, it is refused before
    // anything is prepared or built.
    let s = Sandbox::new();
    let app = app(&s);
    s.write("nm-recipes/app.toml", "[bench.measured]\n  [[bench.measured.step]]\n  lock = \"exclusive\"\n  run = \"true\"\n");
    s.write("two-machines.toml", "[machine.a]\nssh = \"a\"\nhostname = \"a\"\n\n[machine.b]\nssh = \"b\"\nhostname = \"b\"\n");
    let out = s
        .dibs(["bench", &format!("{app}@local"), "measured"])
        .env("DIBS_LOCAL", "0")
        .env("DIBS_MACHINES", s.p("two-machines.toml"))
        .env("DIBS_RECIPES", s.p("nm-recipes"))
        .run();
    assert_eq!(out.code, 2);
    assert_eq!(out.all().lines_with("a measurement names its machine"), 1);
}

#[test]
fn a_sweep_is_one_batch_with_one_summary() {
    // A set of points costs one wake and one summary rather than one of each per point, and the
    // machine sees them in sequence over one worktree.
    let s = Sandbox::new();
    let app = app(&s);
    recipes(&s, PARAMS);
    let local = format!("{app}@local");
    let out = s.dibs(["build", &local, "p", "--sweep", "samples=11,31", "--verbose"]).run();
    assert_eq!(
        (out.stderr.lines_matching("^samples-11 ran cuda samples=11$"), out.stderr.lines_matching("^samples-31 ran cuda samples=31$")),
        (1, 1),
        "a sweep runs one call per value"
    );
    assert_eq!(out.stdout.lines_matching("^batch [0-9-]*  2 steps, "), 1, "as one batch with one summary");
    assert_eq!(out.stdout.lines_matching("^samples-(11|31)  .* 0  "), 2, "each point named by what makes it one");
    let point = runs(&s).lines().filter(|l| l.contains(r#""params":{"backend":"cuda","samples":"11"}"#)).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(point.len(), 1, "and each writes its own record");
    assert_eq!(point[0].lines_matching(r#""batch":"[0-9]{8}-[0-9]{6}-[0-9]+""#), 1, "naming the batch, which is what ties the points of one sweep together");
    let dry = s.dibs(["build", &local, "p", "--sweep", "samples=10,30", "--reps", "2", "--dry-run"]).run().all();
    assert_eq!(dry.lines_matching("^  samples-(10|30) "), 2, "--reps repeats inside each point, which stays one call");
    let out = s.dibs(["build", &local, "p", "--sweep", "backend=cuda,metal"]).run();
    assert_eq!(out.code, 2, "a value the recipe refuses stops the sweep before any of it runs");
    assert_eq!(out.all().lines_matching(r"steps\. You are told"), 0, "without starting the batch");
    assert_eq!(
        s.dibs(["build", &local, "p", "--samples", "10,30", "--dry-run"]).run().stdout.lines_matching("^param       samples = 10,30$"),
        1,
        "a comma in a value is not a sweep"
    );
}

#[test]
fn shell_takes_bench_and_max() {
    // shell is the escape hatch, and a one-off can be a measurement or can outlast the default cap.
    let s = Sandbox::new();
    let app = app(&s);
    let local = format!("{app}@local");
    let out = s.dibs(["shell", &local, "--reason", "measure once", "--bench", "--", "echo measured"]).run();
    assert_eq!(s.log().lines_matching("\tarrived\t.*\tbench\tapp_shell\t"), 1, "shell --bench takes the exclusive lock");
    assert_eq!(out.stdout.lines_matching("^measured$"), 1, "and still runs the command in the tree");
    s.dibs(["shell", &local, "--reason", "long one", "--max", "4242", "--", "true"]).run();
    assert_eq!(s.log().lines_with("holding the lock for 4242s"), 0, "shell --max reaches the lock rather than being dropped");
    assert_eq!(s.dibs(["shell", &local, "--reason", "x", "--max", "4242", "--dry-run", "--", "true"]).code(), 0);
    assert_eq!(
        s.dibs(["shell", &local, "--reason", "measure", "--bench", "--", "cargo bench --bench gemm"]).run().all().lines_with("then measure with --bench"),
        1,
        "a shell that would compile under --bench is refused, with the two calls to use instead"
    );
}

#[test]
fn a_measurement_is_refused_when_another_tree_built_into_its_target() {
    // Commits of one repo share a target, and cargo trusts a source older than its last compile, so
    // a tree checked out before another tree's build measures that tree's binary.
    let s = Sandbox::new();
    let app = app(&s);
    recipes(&s, &gate_recipes(&fake_cargo(&s)));
    let at_main = format!("{app}@main");
    let bench = |extra: &[&str]| {
        let mut args = vec!["bench", at_main.as_str()];
        args.extend_from_slice(extra);
        s.dibs(args).run()
    };
    let out = bench(&["gate"]);
    assert_eq!(
        (out.code, out.all().lines_with("did not make the last build"), out.all().lines_matching("^measured$")),
        (0, 1, 1),
        "a tree that did not make its target's last build is rebuilt, then measured"
    );
    let out = bench(&["gate"]);
    assert_eq!(
        (out.code, out.all().lines_with("did not make the last build"), out.all().lines_matching("^measured$")),
        (0, 0, 1),
        "and a rerun that compiles nothing is measured, and not rebuilt"
    );
    let out = bench(&["stolen"]);
    assert_eq!((out.code, out.all().lines_matching("^measured$")), (78, 0), "a measurement is refused when another tree built into its target since");
    assert_eq!(
        (out.all().lines_with("refused to measure: another tree built into"), out.all().lines_with("pass --anyway")),
        (1, 1),
        "saying why and what to do"
    );
    assert_eq!(out.all().lines_with("  exit 78  by=dibs"), 1, "with dibs named as what ended it");
    assert_eq!(last_run(&s, "app/bench/stolen"), "", "and no record, since nothing was measured");
    let out = bench(&["stolen", "--anyway"]);
    assert_eq!((out.code, out.all().lines_matching("^measured$")), (0, 1), "--anyway measures what is there");
    assert_eq!(last_run(&s, "app/bench/stolen").lines_with(r#""anyway":true"#), 1, "and its record says so");
}

#[test]
fn a_record_carries_every_job_behind_its_number() {
    let s = Sandbox::new();
    let app = app(&s);
    recipes(&s, &format!("{}\n[build.fails]\n  [[build.fails.step]]\n  lock = \"shared\"\n  run = \"exit 3\"\n", gate_recipes(&fake_cargo(&s))));
    for _ in 0..2 {
        s.dibs(["bench", &format!("{app}@main"), "gate"]).run();
    }
    let rec = last_run(&s, "app/bench/gate");
    assert_eq!(
        rec.lines_matching(r#""steps":\[\{"lock":"shared","status":0,"seconds":[0-9]*,"job":"[0-9-]*","built":"nothing","log":"[^"]*:/[^"]*/log"\}"#),
        1,
        "a record carries each step's job, what it built and where its log is: {rec}"
    );
    assert_eq!(rec.lines_matching(r#""state":\{[^}]*"kernel":""#), 1, "the state the machine measured in");
    assert_eq!(rec.lines_matching(r#""outcome":"ok"\}$"#), 1, "and how it ended");
    let listed = s.dibs(["runs", "app/bench/gate"]).run().all();
    assert_eq!(
        listed.lines_matching(r"^[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}  .* app/bench/gate +[0-9]+s exclusive "),
        2,
        "dibs runs gives each run's date, and the measured step's time and lock"
    );
    assert_eq!(listed.lines_matching("^  app/bench/gate on .*: measured 2 times, median "), 1, "and puts runs of one procedure on the same code together");
    s.dibs(["build", &format!("{app}@local"), "fails"]).run();
    assert_eq!(last_run(&s, "app/build/fails").lines_with(r#""outcome":"failed""#), 1, "a failed run is recorded as one");
    assert_eq!(
        (
            s.dibs(["runs", "app/build/fails"]).run().all().lines_with("nothing but failed runs"),
            s.dibs(["runs", "app/build/fails", "--all"]).run().all().lines_matching("FAILED$")
        ),
        (1, 1),
        "and listed only when asked for"
    );
}

#[test]
fn a_recipe_its_series_would_refuse_is_refused_before_it_builds() {
    let s = Sandbox::new();
    let app = app(&s);
    let cargo = fake_cargo(&s);
    recipes(
        &s,
        &format!(
            "{}\n[bench.moved]\n  [[bench.moved.step]]\n  lock = \"shared\"\n  run = \"{cargo} build\"\n  [[bench.moved.step]]\n  lock = \"exclusive\"\n  run = \"echo measured\"\n\n[bench.noted]\n  [[bench.noted.step]]\n  lock = \"shared\"\n  run = \"true\"\n  [[bench.noted.step]]\n  lock = \"exclusive\"\n  run = \"echo measured\"\n",
            gate_recipes(&cargo)
        ),
    );
    let at_main = format!("{app}@main");
    s.dibs(["bench", &at_main, "gate"]).run();
    let here = s.read("series").lines().find_map(|l| l.strip_prefix("app_bench_gate\t").map(|r| r.split('\t').next().unwrap().to_string())).unwrap();
    append(&s.path("series"), &format!("app_bench_moved\t{here}\tgpu:x\tx\t1\t2\n"));
    let n0 = arrivals(&s);
    let out = s.dibs(["bench", &at_main, "moved"]).run();
    assert_eq!(
        (out.code, arrivals(&s) - n0, out.all().lines_with("two histories")),
        (2, 0, 1),
        "a recipe its series would refuse is refused before anything is built"
    );
    let out = s.dibs(["bench", &at_main, "moved", "--new-series"]).run();
    assert_eq!((out.code, out.all().lines_matching("^measured$")), (0, 1), "and --new-series reaches its measurement");
    let card = s.read("series").lines().filter_map(|l| {
        let f: Vec<_> = l.split('\t').collect();
        (f.first() == Some(&"app_bench_moved") && f.get(1) == Some(&here.as_str())).then(|| f[2].to_string())
    }).next_back();
    assert_eq!(card.as_deref(), Some("none"), "starting its series on that machine again");
    assert_eq!(last_run(&s, "app/bench/moved").lines_with(r#""new_series":true"#), 1, "which its record says");
    append(&s.path("series"), "app_bench_noted\tdibs@elsewhere\tnone\tx\t1\t5\n");
    assert_eq!(
        s.dibs(["bench", &at_main, "noted"]).run().all().lines_matching(r"first run of 'app_bench_noted' on .*; its series is on elsewhere \(5 runs\)\.$"),
        1,
        "a recipe's first run on a machine says where its series is, once, before its build"
    );
}

#[test]
fn fresh_gives_each_run_a_value_of_its_own() {
    // An @local tree is reused from run to run, and so is any cache a tool keeps inside it, so a
    // run after an edit would read the autotune winners of the run before.
    let s = Sandbox::new();
    let app = app(&s);
    recipes(&s, "[bench.fresh]\nfresh = [\"STORE\"]\n  [[bench.fresh.step]]\n  lock = \"shared\"\n  run = \"echo build sees $STORE\"\n  [[bench.fresh.step]]\n  lock = \"exclusive\"\n  run = \"echo measure sees $STORE\"\n");
    assert_eq!(s.dibs(["list", &app]).run().stdout.lines_matching("^      fresh each run: STORE$"), 1, "a recipe says what it gives a value of its own each run");
    let local = format!("{app}@local");
    let one = s.dibs(["bench", &local, "fresh"]).run().stdout;
    let two = s.dibs(["bench", &local, "fresh"]).run().stdout;
    let v1 = capture(&one, "^measure sees (.*)$").unwrap();
    assert_eq!(
        (one.lines().filter(|l| *l == format!("build sees {v1}")).count(), v1.split('-').next().unwrap()),
        (1, "dibs"),
        "every step of a run sees one value"
    );
    assert_eq!(two.lines().filter(|l| *l == format!("measure sees {v1}")).count(), 0, "and the next run another");
    assert_eq!(runs(&s).lines_with(&format!(r#""fresh":{{"STORE":"{v1}"}}"#)), 1, "which the record carries");
}

#[test]
fn a_worktree_is_a_line_of_work_on_its_repo_not_a_repo_of_its_own() {
    let mut s = Sandbox::new();
    let app = app(&s);
    recipes(&s, PARAMS);
    s.machines("[machine.lap]\nssh = \"me@lap\"\nhostname = \"lap\"\nmeasure = false\n");
    s.git("app", &["worktree", "add", "-q", &s.p("app-topk")]);
    fs::copy(s.path("app/.dibs.toml"), s.path("app-topk/.dibs.toml")).unwrap();
    s.dibs(["build", &format!("{}@local", s.p("app-topk")), "p"]).run();
    assert_eq!(last_run(&s, "app/build/p").lines_with(r#""repo":"app","variant":"app-topk""#), 1, "a worktree's run records its repo and the tree it came from");
    assert_eq!(s.dibs(["runs", "app/build/p"]).run().stdout.lines_with(" from app-topk"), 1, "and dibs runs says so");
    let affinity = s.path("home/.local/state/dibs/affinity");
    let _ = fs::remove_file(&affinity);
    s.dibs(["build", &format!("{app}@local"), "p"]).env("DIBS_HOST", "lap").run();
    assert_eq!(s.read("home/.local/state/dibs/affinity").lines_matching("^app\tlap\t[0-9]+$"), 1, "an unpinned run says which machine has the repo's cache, and since when");
    let _ = fs::remove_file(&affinity);
    let code = s.dibs(["build", &format!("{app}@local"), "p"]).env("DIBS_ON", "lap").code();
    assert_eq!((code, affinity.exists()), (0, false), "a pinned one does not");
    let variant = |s: &Sandbox| capture(&last_run(s, "app/build/p"), r#"("variant":"[^"]*")"#).unwrap_or_default();
    s.dibs(["build", "app@local", "p"]).dir(&s.path("app-topk")).env("DIBS_ROOT", s.root.display().to_string()).run();
    assert_eq!(variant(&s), r#""variant":"app-topk""#, "a bare name inside a worktree of that repo is the worktree, not the clone under the root");
    s.dibs(["build", &format!("{app}@local"), "p"]).dir(&s.path("app-topk")).env("DIBS_ROOT", s.root.display().to_string()).run();
    assert_eq!(variant(&s), "", "while a path is still that path");
    fs::create_dir_all(s.path("app-topk/crates/x")).unwrap();
    s.dibs(["build", ".@local", "p"]).dir(&s.path("app-topk/crates/x")).env("DIBS_ROOT", s.root.display().to_string()).run();
    assert_eq!(variant(&s), r#""variant":"app-topk""#, "and `.` in a subdirectory of it is that worktree, not the root");
}

#[test]
fn reps_build_once_and_measure_each_time_into_one_record() {
    let s = Sandbox::new();
    let app = app(&s);
    recipes(&s, &gate_recipes(&fake_cargo(&s)));
    let at_main = format!("{app}@main");
    s.dibs(["bench", &at_main, "gate"]).run();
    let n0 = arrivals(&s);
    let out = s.dibs(["bench", &at_main, "gate", "--reps", "3"]).run();
    assert_eq!((out.code, arrivals(&s) - n0, out.all().lines_matching("^measured$")), (0, 4, 3), "--reps builds once and measures each time");
    let rec = last_run(&s, "app/bench/gate");
    let reps = regex::Regex::new(r#""lock":"exclusive","status":0,"seconds":[0-9]*,"rep":[123]"#).unwrap().find_iter(&rec).count();
    assert_eq!((reps, rec.lines_with(r#""reps":3"#)), (3, 1), "into one record whose measurements say which rep they were");
    assert_eq!(s.dibs(["runs", "app/bench/gate"]).run().stdout.lines_matching("median of 3 reps, [0-9]+s to [0-9]+s$"), 1, "and dibs runs gives the spread across them");
}

#[test]
fn a_comparison_is_one_call_measured_against_where_the_branch_left_main() {
    // main moved on after this branch left it, and the local branch named as the base is behind
    // its upstream: comparing against main as it is now would credit the branch with what landed.
    let s = Sandbox::new();
    app(&s);
    let cargo = fake_cargo(&s);
    recipes(
        &s,
        &format!(
            "[bench.ab]\n  [[bench.ab.step]]\n  lock = \"shared\"\n  run = \"{cargo} build\"\n  [[bench.ab.step]]\n  lock = \"exclusive\"\n  run = \"echo measured $(cat a.txt) in ${{CARGO_TARGET_DIR##*/}}\"\n"
        ),
    );
    s.git("app", &["worktree", "add", "-q", &s.p("app-topk")]);
    s.git(".", &["clone", "-q", "-b", "main", "origin.git", "app2"]);
    s.write("app2/a.txt", "main2\n");
    s.git("app2", &["commit", "-qam", "two"]);
    s.git("app2", &["push", "-q", "origin", "HEAD:main"]);
    let old_main = s.git("app", &["rev-parse", "HEAD"]);
    s.git("app-topk", &["fetch", "-q", "origin"]);
    let new_main = s.git("app", &["rev-parse", "origin/main"]);
    s.git("app", &["branch", "-f", "-q", "stale-main", &old_main]);
    s.git("app", &["branch", "-q", "-u", "origin/main", "stale-main"]);
    s.git("app-topk", &["reset", "-q", "--hard", "origin/main"]);
    s.write("app-topk/a.txt", "topk\n");
    fs::copy(s.path("app/.dibs.toml"), s.path("app-topk/.dibs.toml")).unwrap();
    let topk = s.p("app-topk");
    let bench = |refs: &str, extra: &[&str]| {
        let at = format!("{topk}@{refs}");
        let mut args = vec!["bench", at.as_str(), "ab"];
        args.extend_from_slice(extra);
        s.dibs(args).run()
    };
    let dry = bench("stale-main..local", &["--dry-run"]).all();
    assert_eq!(
        (
            dry.lines().filter(|l| *l == format!("arm         base  {new_main}, where local left origin/main, since stale-main is behind it")).count(),
            dry.lines_matching("^arm         local  local ")
        ),
        (1, 1),
        "a dry run of a comparison names each arm and where its base came from:\n{dry}"
    );
    assert_eq!(bench("stale-main..local", &["--reps", "2", "--dry-run"]).all().lines_matching(r"^measured    base local \| local base$"), 1, "and the order they will be measured in");
    let out = bench("stale-main..local", &["--reps", "2"]);
    let measured: Vec<Vec<&str>> = out.stdout.lines().filter(|l| l.starts_with("measured ")).map(|l| l.split(' ').collect()).collect();
    let order: Vec<&str> = measured.iter().map(|f| f[1]).collect();
    assert_eq!((out.code, order), (0, vec!["main2", "topk", "topk", "main2"]), "main..local measures the tree against where it left main, A B B A");
    let target = |arm: &str| {
        let mut t: Vec<String> = measured.iter().filter(|f| f[1] == arm).map(|f| regex::Regex::new("-local-.*").unwrap().replace(f[3], "-local").into_owned()).collect();
        t.dedup();
        t.join(" ")
    };
    assert_eq!((target("main2"), target("topk")), ("app".into(), "app-local".into()), "each arm from a target directory of its own");
    assert_eq!(out.all().lines_matching(r"^  (base |local)  app@.*  [0-9]+s [0-9]+s  jobs [0-9-]+ [0-9-]+$"), 2, "with a summary naming each arm's jobs");
    let rec = last_run(&s, "app/bench/ab");
    let arms = format!(
        r#""refs":"stale-main..local","arms":[{{"name":"base","fetched":"{new_main}","revisions":{{"app":"{}"}}}},{{"name":"local","revisions":{{"app":"local:"#,
        &new_main[..12]
    );
    assert!(rec.contains(&arms), "one record names both arms and what each resolved to: {rec}");
    assert_eq!(regex::Regex::new(r#""arm":"(base|local)""#).unwrap().find_iter(&rec).count(), 6, "and tags every step with its arm");
    let listed = s.dibs(["runs", "app/bench/ab"]).run().all();
    assert_eq!(
        (listed.lines_with("app/bench/ab  stale-main..local arms, 2 reps each"), listed.lines_matching("^    (base |local)  app@")),
        (1, 2),
        "dibs runs lists a comparison arm by arm"
    );
    let out = bench(&format!("{old_main},{new_main}"), &[]);
    let pairs: Vec<String> = out.stdout.lines().filter(|l| l.starts_with("measured ")).map(|l| {
        let f: Vec<_> = l.split(' ').collect();
        format!("{} {}", f[1], f[3])
    }).collect();
    assert_eq!((out.code, pairs.join(" ")), (0, "x app main2 app-arm1".into()), "a list compares each in turn, a later fetched arm in a target of its own");
    assert_eq!(bench("main...local", &[]).code, 2, "a range with no base named is refused");
    assert_eq!(bench("local,local", &[]).code, 2, "an arm named twice is refused");
    assert_eq!(bench(&format!("origin/main..{new_main}"), &[]).all().lines_with("nothing to compare"), 1, "a tip with nothing its base lacks is refused");
}

#[test]
fn a_recipes_artifacts_come_back_by_themselves() {
    // Each step keeps the files it wrote in its job directory and the run fetches them, so nothing
    // is left on the machine to be copied by hand.
    let s = Sandbox::new();
    let app = app(&s);
    s.write("app/results/old.json", "stale\n");
    recipes(
        &s,
        &format!(
            "{}\n[bench.art]\nartifacts = [\"results/*.json\", \"$CARGO_TARGET_DIR/crit/**/est.json\"]\n  [[bench.art.step]]\n  lock = \"shared\"\n  run = \"mkdir -p $CARGO_TARGET_DIR/crit/g && echo e > $CARGO_TARGET_DIR/crit/g/est.json\"\n  [[bench.art.step]]\n  lock = \"exclusive\"\n  run = \"echo measured > results/new.json\"\n\n[bench.badart]\nartifacts = [\"results/a b.json\"]\n  [[bench.badart.step]]\n  lock = \"exclusive\"\n  run = \"true\"\n",
            gate_recipes(&fake_cargo(&s))
        ),
    );
    let local = format!("{app}@local");
    let state = s.p("home/.local/state/dibs");
    let out = s.dibs(["bench", &local, "art", "--artifacts", &s.p("got")]).run();
    assert_eq!(
        (out.code, s.read("got/results/new.json"), s.read("got/target/crit/g/est.json")),
        (0, "measured\n".to_string(), "e\n".to_string()),
        "the files a run wrote come back into the directory named, at their paths"
    );
    let kept: Vec<_> = fs::read_dir(s.path("got/results")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(kept, ["new.json"], "and none an earlier run left, even one sent with the tree");
    assert_eq!(
        out.all().lines_matching(&format!("^dibs: 1 file\\(s\\) from job [0-9-]*, kept in {state}/jobs/[0-9-]*/artifacts$")),
        2,
        "saying where each job's are kept"
    );
    let rec = last_run(&s, "app/bench/art");
    assert_eq!(rec.matches(r#""artifacts":1"#).count(), 2, "and the record counts what each step kept");
    let job = regex::Regex::new(r#""artifacts":1,"job":"([0-9-]*)""#).unwrap().captures_iter(&rec).last().unwrap()[1].to_string();
    fs::remove_dir_all(format!("{state}/jobs/{job}")).unwrap();
    let out = s.dibs(["--fetch", &job, &s.p("got2")]).run();
    assert_eq!((out.code, s.read("got2/results/new.json")), (0, "measured\n".to_string()), "dibs --fetch brings a job's files back from the machine");
    assert!(s.exists(&format!("home/.local/state/dibs/jobs/{job}/artifacts/results/new.json")), "and keeps them for the next time");
    s.dibs(["bench", &local, "gate"]).run();
    let gate_job = regex::Regex::new(r#""job":"([0-9-]*)""#).unwrap().captures_iter(&last_run(&s, "app/bench/gate")).last().unwrap()[1].to_string();
    let out = s.dibs(["--fetch", &gate_job]).run();
    assert_eq!((out.code, out.all().lines_with("kept no files")), (3, 1), "a job that kept nothing says so");
    s.dibs(["bench", &local, "art", "--reps", "2", "--artifacts", &s.p("got3")]).run();
    let found = s.command("bash", ["-c", "cd got3 && find . -type f | sort | paste -sd' '"]).run().stdout;
    assert_eq!(found.trim_end(), "./r1/results/new.json ./r2/results/new.json ./target/crit/g/est.json", "reps come back apart, and the build's once");
    assert_eq!(s.dibs(["bench", &local, "badart"]).code(), 2, "a pattern the shell would split is refused");
}

#[test]
fn a_pin_builds_against_another_trees_unpushed_changes() {
    // With a real cargo: what is checked is that cargo reads the patch dibs puts above the tree.
    // The toolchain's own cargo, ahead of any wrapper on PATH, since a wrapper that gates builds
    // expects the real home and session and hangs under the sandbox's.
    let mut s = Sandbox::new();
    let app = app(&s);
    let real_home = std::env::var("HOME").unwrap();
    if s.var("RUSTUP_HOME").is_empty() {
        s.set("RUSTUP_HOME", format!("{real_home}/.rustup"));
    }
    let toolchain = format!("{real_home}/.cargo/bin");
    if std::path::Path::new(&format!("{toolchain}/cargo")).exists() {
        let path = s.var("PATH");
        let (ours, rest) = path.split_once(':').unwrap();
        s.set("PATH", format!("{ours}:{toolchain}:{rest}"));
    }
    s.git(".", &["init", "-q", "--bare", "lib.git"]);
    s.git(".", &["clone", "-q", "lib.git", "lib"]);
    s.git("lib", &["checkout", "-q", "-b", "main"]);
    s.write("lib/Cargo.toml", "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    s.write("lib/src/lib.rs", "pub fn say() -> &'static str { \"pushed\" }\n");
    s.write("lib/.gitignore", "target\n");
    s.git("lib", &["add", "-A"]);
    s.git("lib", &["commit", "-qm", "lib"]);
    s.git("lib", &["push", "-q", "origin", "main"]);
    s.git(".", &["init", "-q", "consumer"]);
    s.write("consumer/.gitignore", "target\n");
    s.write("consumer/Cargo.toml", "[workspace]\nmembers = [\"bin\"]\nresolver = \"2\"\n");
    s.write(
        "consumer/bin/Cargo.toml",
        &format!("[package]\nname = \"bin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nlib = {{ git = \"file://{}\", branch = \"main\", version = \"0.1\" }}\n", s.p("lib.git")),
    );
    s.write("consumer/bin/src/main.rs", "fn main() { println!(\"{}\", lib::say()); }\n");
    assert_eq!(s.command("cargo", ["generate-lockfile", "-q"]).dir(&s.path("consumer")).code(), 0);
    s.git("consumer", &["add", "-A"]);
    s.git("consumer", &["commit", "-qm", "consumer"]);
    s.write("consumer/.dibs.toml", "[build.say]\n  [[build.say.step]]\n  lock = \"shared\"\n  run = \"cargo build -q && $CARGO_TARGET_DIR/debug/bin\"\n");
    s.write("lib/src/lib.rs", "pub fn say() -> &'static str { \"unpushed\" }\n");
    let consumer = format!("{}@local", s.p("consumer"));
    let lib = format!("{}@local", s.p("lib"));
    let build = |extra: &[&str]| {
        let mut args = vec!["build", consumer.as_str(), "say"];
        args.extend_from_slice(extra);
        s.dibs(args).run()
    };
    let out = build(&[]);
    assert_eq!((out.code, out.all().lines_matching("^pushed$")), (0, 1), "without a pin, the build takes the pushed revision: {}", out.all());
    let out = build(&["--pin", &lib]);
    assert_eq!((out.code, out.all().lines_matching("^unpushed$")), (0, 1), "with one, it builds against the tree here, unpushed changes included: {}", out.all());
    let scratch = s.var("DIBS_SCRATCH");
    let nest = fs::read_dir(format!("{scratch}/ws/consumer")).unwrap().flatten().map(|e| e.path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with("pin-")).unwrap();
    let config = fs::read_to_string(nest.join(".cargo/config.toml")).unwrap_or_default();
    let sent = fs::read_dir(&nest).unwrap().flatten().find(|e| e.file_name().to_string_lossy().starts_with("local-")).unwrap().path();
    assert_eq!(
        (config.lines_with(&format!("lib = {{ path = \"{scratch}/ws/lib/local-")), fs::read_to_string(sent.join("Cargo.toml")).unwrap().lines_with("patch")),
        (1, 0),
        "through a patch above the tree, which stays what was sent"
    );
    assert_eq!(
        last_run(&s, "consumer/build/say").lines_matching(r#""revisions":\{"consumer":"local:[^"]*","lib":"local:"#),
        1,
        "and the record names the pinned tree's revision"
    );
    assert_eq!(
        build(&["--pin", &lib, "--dry-run"]).stdout.lines().filter(|l| *l == format!("            patches file://{}: lib", s.p("lib.git"))).count(),
        1,
        "a dry run says what a pin replaces"
    );
    s.write("lib/Cargo.toml", "[package]\nname = \"lib\"\nversion = \"0.2.0\"\nedition = \"2021\"\n");
    let out = build(&["--pin", &lib]);
    assert_eq!(
        (out.code, out.all().lines_with("the pin did not take"), out.all().lines_matching("^  lib from git\\+file://")),
        (3, 1, 1),
        "a pin whose version the requirement refuses fails rather than building the pushed code"
    );
    assert_eq!(build(&["--pin", &consumer]).code, 2, "pinning the repo being built is refused");
    assert_eq!(build(&["--pin", &format!("{app}@local")]).all().lines_with("nothing"), 1, "and so is a pin its lockfile has no use for");
}

#[test]
fn what_got_in_the_way_is_filed_and_counted() {
    // The report people write starts with the flag they are complaining about, so the text travels
    // in the environment: every parser between the shell and the file would claim it.
    let s = Sandbox::new();
    let friction = || s.read("home/.local/state/dibs/friction.jsonl");
    s.dibs(["--friction", "--stream does nothing inside a batch"]).run();
    assert_eq!(friction().lines_with(r#""text":"--stream does nothing inside a batch""#), 1, "a report about a flag keeps the flag");
    assert_eq!(friction().lines_with(r#""by":""#), 1, "and names the session, so it can be asked what it was doing");
    s.dibs(["friction", "the trailer says built=nothing but the recipe did build"]).run();
    s.dibs(["--friction", "--stream does nothing inside a batch."]).run();
    let gaps = s.dibs(["gaps"]).run().all();
    assert_eq!(gaps.lines_with("What got in the way"), 1, "gaps prints it beside what did not fit a recipe");
    // One report is a nuisance somebody worked around; the same one three times specifies a fix.
    assert_eq!(gaps.lines_with("2x  --stream does nothing"), 1, "and counts the same thing said twice as twice");
    assert_eq!(s.dibs(["--friction", "   "]).code(), 2, "an empty report is refused rather than filed");
}
