//! The runner's own tree, which machines build: it has to agree with the workspace it comes from.

use crate::harness::*;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn toml(rel: &str) -> toml::Table {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .parse()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn a_change_to_any_file_of_the_runners_tree_repacks_it() {
    let output = Path::new(env!("OUT_DIR")).join("../output");
    let output = fs::read_to_string(&output).unwrap();
    let watched: Vec<PathBuf> = output
        .lines()
        .filter_map(|line| line.strip_prefix("cargo:rerun-if-changed="))
        .map(PathBuf::from)
        .collect();
    let s = Sandbox::new();
    fs::write(s.path("tree.tar.gz"), SOURCE).unwrap();
    let listed = s.sh("tar -tzf tree.tar.gz").run();
    assert_eq!(listed.code, 0, "{}", listed.all());
    let root = repo_root().canonicalize().unwrap();
    for packed in listed.stdout.lines() {
        let from = match packed {
            "Cargo.toml" => "crates/dibs-runner/provision/workspace.toml",
            "Cargo.lock" => "crates/dibs-runner/provision/Cargo.lock",
            "install.sh" => "crates/dibs-runner/provision/install.sh",
            other => other,
        };
        let file = root.join(from);
        assert!(
            watched.iter().any(|w| file.starts_with(w)),
            "{from} is packed into the runner's tree, but a change to it would not repack the tree"
        );
    }
}

#[test]
fn the_runners_workspace_builds_as_the_workspace_does() {
    let workspace = toml("Cargo.toml");
    let runner = toml("crates/dibs-runner/provision/workspace.toml");
    let section = |t: &toml::Table, key: &str| t["workspace"][key].clone();
    for key in ["package", "lints"] {
        assert_eq!(
            section(&runner, key),
            section(&workspace, key),
            "[workspace.{key}]"
        );
    }
    assert_eq!(runner["profile"], workspace["profile"], "[profile]");
    let dependencies = section(&runner, "dependencies");
    for (name, wanted) in dependencies.as_table().unwrap() {
        if name != "dibs-format" {
            assert_eq!(
                Some(wanted),
                section(&workspace, "dependencies").get(name),
                "{name}"
            );
        }
    }
}

/// A cargo that builds nothing: it checks it was run in the tree, under the shared lock and named
/// by a holder, runs `$CARGO_ALSO` where a test gives one, then puts the runner this suite runs
/// where cargo would have built it.
const FAKE_CARGO: &str = r#"#!/bin/bash
[ "$*" = "build --locked --release" ] || { echo "cargo: not the build expected: $*" >&2; exit 2; }
[ -f Cargo.lock ] && [ -f install.sh ] && [ -f crates/dibs-runner/src/lib.rs ] || { echo "cargo: no runner tree here" >&2; exit 101; }
perl -MFcntl=:flock -e 'open(my $f, ">>", $ARGV[0]) or exit 0; exit(flock($f, LOCK_EX | LOCK_NB) ? 1 : 0)' "$DIBS_LOCK_DIR/rw" || { echo "cargo: built outside the lock" >&2; exit 101; }
grep -qs "dibs-runner" "$DIBS_LOCK_DIR"/holder.* || { echo "cargo: no holder names the build" >&2; exit 101; }
[ -z "${FAIL_BUILD:-}" ] || { echo "error: could not compile dibs-runner" >&2; exit 101; }
[ -z "${CARGO_ALSO:-}" ] || bash "$CARGO_ALSO"
mkdir -p "${CARGO_TARGET_DIR:-target}/release"
cp "$PREBUILT" "${CARGO_TARGET_DIR:-target}/release/dibs-runner"
"#;

/// A machine whose only runner is an older one, or none, and whose cargo is the fake.
fn without_this_runner(s: &mut Sandbox, older: bool) {
    fs::remove_file(s.path(&format!("home/{}", runner_path()))).unwrap();
    if older {
        s.write_exec(
            "home/.cache/dibs/runner/0000000000000000/dibs-runner",
            &prebuilt_runner(),
        );
    }
    s.write_exec("home/.cargo/bin/cargo", FAKE_CARGO);
    s.write_exec("prebuilt", &prebuilt_runner());
    s.set("PREBUILT", s.p("prebuilt"));
}

fn installed(s: &Sandbox) -> bool {
    s.exists(&format!("home/{}", runner_path()))
}

#[test]
fn a_machine_without_this_runner_has_the_one_it_has_build_it() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, true);
    let out = s
        .remote(s.dibs(["--label", "after-build", "echo ran"]))
        .run();
    assert_eq!(
        (out.code, out.stdout.lines_with("ran")),
        (0, 1),
        "the call runs once its runner is built: {}",
        out.all()
    );
    assert!(installed(&s), "and the runner stays for the next call");
    s.log_line("finished\t[0-9]+\tshared\tdibs-runner\t");
    assert_eq!(
        out.stderr.lines_with("Building it there as a shared job"),
        1,
        "the caller is told why it waits: {}",
        out.stderr
    );
    let again = s
        .remote(s.dibs(["--label", "after-build", "echo ran"]))
        .run();
    assert_eq!(
        (again.code, again.stderr.lines_with("Building")),
        (0, 0),
        "and the next call builds nothing: {}",
        again.all()
    );
}

#[test]
fn shared_work_naming_no_machine_is_placed_on_one_without_this_runner_and_builds_it() {
    let mut s = Sandbox::new();
    s.machines(
        "[machine.there]\nssh      = \"there\"\nhostname = \"there\"\n\n[machine.gone]\nssh      = \"nowhere.invalid\"\nhostname = \"gone\"\n",
    );
    without_this_runner(&mut s, true);
    let placed = || {
        s.remote(s.dibs(["--label", "placed", "echo ran"]))
            .env("DIBS_LOCAL", "0")
            .env("DIBS_CONNECT_TIMEOUT", "2")
            .run()
    };
    let out = placed();
    assert_eq!(
        (out.code, out.stdout.lines_with("ran")),
        (0, 1),
        "{}",
        out.all()
    );
    assert!(installed(&s), "{}", out.all());
    assert_eq!(placed().code, 0, "and it is not taken for down afterwards");
}

#[test]
fn a_runner_that_does_not_build_stops_the_call_with_72() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, true);
    s.set("FAIL_BUILD", "1");
    let out = s.remote(s.dibs(["--label", "never", "echo ran"])).run();
    assert_eq!(out.code, 72, "{}", out.all());
    assert_eq!(out.stdout.lines_with("ran"), 0, "nothing ran");
    assert_eq!(
        (
            out.all().lines_with("could not compile"),
            out.stderr.lines_with("could not be built")
        ),
        (1, 1),
        "and the build's own reason is shown: {}",
        out.all()
    );
    assert!(!installed(&s));
}

#[test]
fn a_runner_that_passes_its_check_and_cannot_exec_is_built_again() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, true);
    // It passes the far shell's `[ -x ]`, and its exec fails with 126; aged, so the older runner
    // is the newest one there to build.
    let runner = format!("home/{}", runner_path());
    s.write_exec(&runner, "\u{7f}ELF\0\0\0\0");
    s.command("touch", ["-d", "400 days ago", &s.p(&runner)])
        .run();
    let out = s
        .remote(s.dibs(["--label", "after-build", "echo ran"]))
        .run();
    assert_eq!(
        (out.code, out.stdout.lines_with("ran")),
        (0, 1),
        "the call runs once its runner is built: {}",
        out.all()
    );
    let installed = fs::symlink_metadata(s.path(&format!("home/{}", runner_path()))).unwrap();
    assert!(installed.is_file());
}

// macOS starts a queued gc's runner by its path (protocol.md, Platforms).
#[cfg(target_os = "linux")]
#[test]
fn a_queued_gc_starts_though_its_runner_was_removed_while_it_waited() {
    let mut s = Sandbox::new();
    // A binary of its own in the version's directory, as a built runner is.
    let version = format!("home/.cache/dibs/runner/{RUNNER_HASH}");
    let copy = s.p(&format!("{version}/dibs-copy"));
    s.command("cp", [DIBS, copy.as_str()]).run();
    s.write_exec(
        &format!("home/{}", runner_path()),
        &format!("#!/bin/sh\nexec '{copy}' __runner \"$@\"\n"),
    );
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let bench = s.spawn(s.dibs([
        "--bench",
        "--label",
        "measured",
        &format!("{}; {}", up.signal(), hold.hold()),
    ]));
    up.reached();
    let gc = s.spawn(s.remote(s.dibs(["--gc", "--dry-run"])));
    s.until_records("the sweep queued", || {
        s.records("waiting")
            .iter()
            .any(|r| r.get(3).is_some_and(|label| label == "dibs-gc"))
    });
    fs::remove_dir_all(s.path(&version)).unwrap();
    hold.open();
    assert_eq!(s.wait(bench), 0);
    assert_eq!(s.wait(gc), 0);
}

#[test]
fn a_machine_with_no_runner_refuses_the_call_and_says_how_to_install_one() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, false);
    let out = s.remote(s.dibs(["--label", "never", "echo ran"])).run();
    assert_eq!(out.code, 72, "{}", out.all());
    assert_eq!(out.stderr.lines_with("dibs --check"), 1, "{}", out.stderr);
    assert_eq!(
        out.stderr.lines_with("Building"),
        0,
        "no build is announced with nothing there to build it: {}",
        out.stderr
    );
    assert_eq!(
        s.log().lines_with("never"),
        0,
        "and nothing reached the lock"
    );
}

#[test]
fn a_question_within_a_bound_builds_no_runner() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, true);
    s.machines("[machine.box-a]\nssh = \"dibs@box-a\"\nhostname = \"box-a\"\n");
    let out = s.remote(s.dibs(["status", "--all"])).run();
    assert_eq!(
        out.all()
            .lines_with("no runner for this dibs yet, so it was not asked"),
        1,
        "{}",
        out.all()
    );
    assert!(!installed(&s), "nothing was built");
    assert_eq!(s.log().lines_with("dibs-runner"), 0, "nor queued");
}

/// The source of a runner left under this build's hash by a cargo that copied a stale binary.
const OTHER_SOURCE: &str = "0000000000000000";

/// A runner of other source: it names that source, and serves no call for this one.
fn stale_runner() -> String {
    format!(
        "#!/bin/sh\ncase $1 in\nhash) echo {OTHER_SOURCE} ;;\n\
         serve) echo \"dibs-runner: this is the runner of {OTHER_SOURCE}, not of $2.\" >&2; exit 125 ;;\n\
         *) exec '{DIBS}' __runner \"$@\" ;;\nesac\n"
    )
}

#[test]
fn a_runner_of_other_source_under_this_hash_is_built_over() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, false);
    s.write_exec(&format!("home/{}", runner_path()), &stale_runner());
    let out = s.remote(s.dibs(["--label", "after", "echo ran"])).run();
    assert_eq!(
        (out.code, out.stdout.lines_with("ran")),
        (0, 1),
        "{}",
        out.all()
    );
    let named = s
        .command(&s.p(&format!("home/{}", runner_path())), ["hash"])
        .run();
    assert_eq!(named.stdout.trim_end(), RUNNER_HASH, "{}", out.all());
}

#[test]
fn a_build_that_makes_a_runner_of_other_source_installs_nothing() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, true);
    s.write_exec("prebuilt", &stale_runner());
    let out = s.remote(s.dibs(["--label", "never", "echo ran"])).run();
    assert_eq!(out.code, 72, "{}", out.all());
    assert_eq!(
        out.stderr
            .lines_with(&format!("runner of {OTHER_SOURCE} where")),
        1,
        "{}",
        out.stderr
    );
    assert!(!installed(&s));
}

/// The tree this build carries, as it goes to a machine.
const SOURCE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runner-source.tar.gz"));

#[test]
#[ignore = "builds the runner twice with the real cargo, which CI's runner-build job does"]
fn a_second_source_is_built_into_the_target_the_first_left() {
    let mut s = Sandbox::new();
    s.real_cargo();
    fs::remove_file(s.path(&format!("home/{}", runner_path()))).unwrap();
    fs::write(s.path("first.tar.gz"), SOURCE).unwrap();
    let repacked = s
        .sh("mkdir second && tar -xzf first.tar.gz -C second \
             && echo '// a second source' >> second/crates/dibs-format/src/lib.rs \
             && TZ=UTC find second -exec touch -t 197001010000 {} + \
             && tar -czf second.tar.gz -C second .")
        .run();
    assert_eq!(repacked.code, 0, "{}", repacked.all());
    let second = format!("{:016x}", u64::from_str_radix(RUNNER_HASH, 16).unwrap() ^ 1);
    for (hash, tree) in [
        (RUNNER_HASH, "first.tar.gz"),
        (second.as_str(), "second.tar.gz"),
    ] {
        let built = s
            .sh(&format!("dibs __runner build {hash} < {tree}"))
            .within(Duration::from_secs(1800))
            .run();
        assert_eq!(built.code, 0, "{}", built.all());
        let runner = s.p(&format!("home/.cache/dibs/runner/{hash}/dibs-runner"));
        let named = s.command(&runner, ["hash"]).run();
        assert_eq!(
            named.stdout.trim_end(),
            hash,
            "what is installed for {hash} was built from it: {}",
            built.all()
        );
    }
}

#[test]
fn check_installs_the_first_runner_on_a_machine_with_none() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, false);
    let out = s.remote(s.dibs(["--check"])).run();
    assert!(installed(&s), "the runner is installed: {}", out.all());
    assert_eq!(out.stderr.lines_with("a first build"), 1, "{}", out.stderr);
    let after = s.remote(s.dibs(["--label", "after", "echo ran"])).run();
    assert_eq!(
        (after.code, after.stdout.lines_with("ran")),
        (0, 1),
        "{}",
        after.all()
    );
}

#[test]
fn the_first_build_queues_behind_a_benchmark() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, false);
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let bench = s.spawn(s.dibs([
        "--bench",
        "--label",
        "measured",
        &format!("{}; {}", up.signal(), hold.hold()),
    ]));
    up.reached();
    s.held(1);
    let check = s.spawn(s.remote(s.dibs(["--check"])));
    s.until_records("the first build queued", || {
        s.records("waiting")
            .iter()
            .any(|r| r.get(3).is_some_and(|label| label == "dibs-runner"))
    });
    assert!(!installed(&s), "nothing is built beside the benchmark");
    hold.open();
    assert_eq!(s.wait(bench), 0);
    s.wait(check);
    assert!(installed(&s), "the build ran once the benchmark let go");
    assert_eq!(
        (s.holders(), s.waiters()),
        (0, 0),
        "and its records went with it"
    );
}

#[test]
fn what_the_first_build_leaves_running_holds_no_lock() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, false);
    let (escaped, daemon) = (s.gate("escaped"), s.gate("daemon"));
    s.write(
        "daemon.sh",
        &format!(
            "echo $$ > {}; {}; {}\n",
            s.p("daemon.pid"),
            escaped.signal(),
            daemon.hold()
        ),
    );
    s.write(
        "also.sh",
        &format!(
            "perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' bash {} </dev/null >/dev/null 2>&1 &\n{}\n",
            s.p("daemon.sh"),
            escaped.hold()
        ),
    );
    s.set("CARGO_ALSO", s.p("also.sh"));
    let out = s.remote(s.dibs(["--check"])).run();
    assert!(installed(&s), "{}", out.all());
    let left: u32 = s.read("daemon.pid").trim().parse().unwrap();
    assert!(alive(left), "the daemon outlived the build");
    let measured = s
        .dibs([
            "--bench",
            "--wait",
            "2",
            "--label",
            "after",
            "echo measured",
        ])
        .run();
    assert_eq!(
        measured.code,
        0,
        "a daemon the build started, as sccache starts one, keeps no lock: {}",
        measured.all()
    );
    daemon.open();
}

/// How a first build is stopped while cargo runs.
enum Stop {
    Killed,
    CallerGone,
}

/// Which build: the first, which perl locks, or one a runner already there makes.
enum Build {
    First,
    ByNewest,
}

#[test]
fn a_first_build_told_to_stop_takes_its_whole_build_with_it() {
    build_stopped(Build::First, Stop::Killed);
}

#[test]
fn a_first_build_whose_caller_goes_takes_its_whole_build_with_it() {
    build_stopped(Build::First, Stop::CallerGone);
}

#[test]
fn a_build_told_to_stop_takes_its_whole_build_with_it() {
    build_stopped(Build::ByNewest, Stop::Killed);
}

#[test]
fn a_build_whose_caller_goes_takes_its_whole_build_with_it() {
    build_stopped(Build::ByNewest, Stop::CallerGone);
}

fn build_stopped(build: Build, stop: Stop) {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, matches!(build, Build::ByNewest));
    let (up, never) = (s.gate("up"), s.gate("never"));
    s.write(
        "also.sh",
        &format!(
            "echo $$ > {}; {}; {}\n",
            s.p("cargo.pid"),
            up.signal(),
            never.hold()
        ),
    );
    s.set("CARGO_ALSO", s.p("also.sh"));
    let call = match build {
        Build::First => s.dibs(["--check"]),
        Build::ByNewest => s.dibs(["--label", "after", "echo ran"]),
    };
    let check = s.spawn(s.remote(call));
    up.reached();
    let cargo: u32 = s.read("cargo.pid").trim().parse().unwrap();
    match stop {
        Stop::Killed => {
            let holder = s.pid_of("dibs-runner");
            let killed = s.dibs(["--kill", &holder.to_string(), "--anyone"]).run();
            assert_eq!(killed.code, 0, "{}", killed.all());
        }
        // SAFETY: kill only signals the client this test started.
        Stop::CallerGone => unsafe {
            libc::kill(check.pid as i32, libc::SIGKILL);
        },
    }
    s.wait(check);
    let sent = || {
        fs::read_dir(s.path("home/.cache/dibs/runner"))
            .unwrap()
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with(".src.") || name.starts_with(".tree.")
            })
            .count()
    };
    until(
        "the build, its records and the tree it was sent to go",
        || !alive(cargo) && s.holders() == 0 && s.waiters() == 0 && sent() == 0,
    );
    assert!(!installed(&s), "and nothing was installed");
}

#[test]
fn builds_of_two_runners_take_turns_with_the_target_they_share() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, false);
    fs::write(s.path("tree.tar.gz"), SOURCE).unwrap();
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    s.write(
        "also.sh",
        &format!(
            "mkdir {first} 2>/dev/null || {{ echo second-start >> {order}; exit 0; }}\n\
             echo first-start >> {order}; {}; {}; echo first-end >> {order}\n",
            up.signal(),
            hold.hold(),
            first = s.p("first"),
            order = s.p("order"),
        ),
    );
    s.set("CARGO_ALSO", s.p("also.sh"));
    let tree = s.p("tree.tar.gz");
    let build = |hash: &str| format!("dibs __runner build {hash} < {tree}");
    let first = s.spawn(s.sh(&build(RUNNER_HASH)));
    up.reached();
    let other = format!("{:016x}", u64::from_str_radix(RUNNER_HASH, 16).unwrap() ^ 1);
    let (out, err) = (s.path("second.out"), s.path("second.err"));
    let second = s.spawn(s.sh(&build(&other)).streams_to(&out, &err));
    until("the second build to wait its turn", || {
        fs::read_to_string(&err).is_ok_and(|e| e.contains("this one waits for it"))
    });
    hold.open();
    assert_eq!(s.wait(first), 0);
    s.wait(second);
    assert_eq!(
        s.read("order"),
        "first-start\nfirst-end\nsecond-start\n",
        "the second build's cargo ran once the first had installed"
    );
}

/// Every command on the suite's PATH but perl, linked into one directory.
fn tools_without_perl(s: &Sandbox) -> String {
    let tools = s.path("noperl");
    fs::create_dir_all(&tools).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    for entry in path
        .split(':')
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flatten()
    {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("perl") && !tools.join(&name).exists() {
            let _ = std::os::unix::fs::symlink(entry.path(), tools.join(&name));
        }
    }
    tools.display().to_string()
}

#[test]
fn a_machine_without_perl_gets_no_first_build_outside_the_lock() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, false);
    let tools = tools_without_perl(&s);
    if !s.exists("noperl/setsid") {
        skip("the fake ssh needs setsid or perl");
        return;
    }
    s.set("PATH", format!("{}:{}:{tools}", s.p("bin"), s.p("nossh")));
    let out = s.remote(s.dibs(["--check"])).run();
    assert_eq!(out.code, 72, "{}", out.all());
    assert_eq!(out.stderr.lines_with("has no perl"), 1, "{}", out.stderr);
    assert!(!installed(&s), "nothing was built");
}

#[test]
fn the_accounts_settings_file_comes_before_the_machines_and_its_environment() {
    let s = Sandbox::new();
    s.write("etc/runner.toml", "keep_days = 7\n");
    s.write(
        "home/.config/dibs/runner.toml",
        "# the sweep's clock\nkeep_days = 3\n",
    );
    let out = s
        .dibs(["--gc", "--dry-run"])
        .env("DIBS_KEEP_DAYS", "9")
        .run();
    assert_eq!(
        out.stdout
            .lines_with("job logs and artifacts, removed after 3 days"),
        1,
        "{}",
        out.stdout
    );
}

#[test]
fn how_quick_a_job_must_be_to_go_around_is_the_machines_alone() {
    let mut s = Sandbox::new();
    s.write("etc/runner.toml", "quick = 5\n");
    s.write(
        "home/.config/dibs/runner.toml",
        "quick = 600\n[runner]\nquik = 600\n",
    );
    s.history("shared\ttens\t10\nshared\ttens\t10\nshared\ttens\t10\n");
    let (anchor, blocked, tens) = (s.gate("anchor"), s.gate("blocked"), s.gate("tens"));
    let a = s.spawn(s.dibs(["--label", "anchor", &anchor.hold()]));
    s.held(1);
    let b = s.spawn(s.dibs(["--bench", "--label", "blocked", &blocked.hold()]));
    s.queued(1);
    let t = s.spawn(s.dibs(["--label", "tens", &tens.hold()]));
    s.queued(2);
    assert_eq!(
        (s.holders(), s.waiters()),
        (1, 2),
        "a ten-second job waits, which this account's quick = 600 would have sent around"
    );
    let check = s.dibs(["--check"]).run();
    for said in [
        "sets quick, which only",
        "holds \"[runner]\"",
        "holds \"quik = 600\"",
    ] {
        assert_eq!(check.stdout.lines_with(said), 1, "{said}: {}", check.stdout);
    }
    anchor.open();
    blocked.open();
    tens.open();
    for job in [a, b, t] {
        assert_eq!(s.wait(job), 0);
    }
}

#[test]
fn the_settings_file_cannot_move_the_lock() {
    let s = Sandbox::new();
    s.write(
        "home/.config/dibs/runner.toml",
        &format!("lock_dir = \"{}\"\n", s.p("elsewhere")),
    );
    let out = s.dibs(["--label", "held", "echo ran"]).run();
    assert_eq!(out.code, 0, "{}", out.all());
    assert!(
        !s.exists("elsewhere"),
        "no lock was taken where the file says"
    );
    s.log_line("finished\t[0-9]+\tshared\theld\t");
    let refused = "sets lock_dir, which only the environment sets";
    assert_eq!(out.stderr.lines_with(refused), 1, "{}", out.stderr);
    let check = s.dibs(["--check"]).run();
    assert_eq!(check.stdout.lines_with(refused), 1, "{}", check.stdout);
}

#[test]
fn a_machine_holding_series_refuses_another_card_whoever_runs_it() {
    let s = Sandbox::new();
    let cards = s.path("cards");
    fs::write(&cards, "#dibs-cards 1\nmoved\tgpu:x\tsomeone else\t1\t2\n").unwrap();
    let run = |extra: &[&str], on: bool| {
        let mut args = vec!["--bench", "--label", "moved"];
        args.extend(extra);
        args.push("true");
        let call = s.dibs(args).env("DIBS_SERIES_CHECK", "0");
        match on {
            true => call.env("DIBS_MACHINE_SERIES", "1").run(),
            false => call.run(),
        }
    };
    assert_eq!(run(&[], false).code, 0, "off until the machine turns it on");
    let refused = run(&[], true);
    assert_eq!(refused.code, 2, "{}", refused.stderr);
    assert_eq!(
        refused.stderr.lines_with("before:  gpu:x, by someone else"),
        1,
        "and says who measured it before"
    );
    assert_eq!(run(&["--new-series"], true).code, 0);
    assert_eq!(
        fs::read_to_string(&cards)
            .unwrap()
            .lines_with("moved\tnone\t"),
        1,
        "--new-series binds it to this run's card"
    );
    assert_eq!(run(&[], true).code, 0, "which the next run keeps to");
}
