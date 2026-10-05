//! The runner's own tree, which machines build: it has to agree with the workspace it comes from.

use crate::harness::*;
use std::{collections::BTreeSet, fs, time::Duration};

fn toml(rel: &str) -> toml::Table {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .parse()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every package a lock file pins, as `name version`.
fn pinned(rel: &str) -> BTreeSet<String> {
    toml(rel)["package"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            format!(
                "{} {}",
                p["name"].as_str().unwrap(),
                p["version"].as_str().unwrap()
            )
        })
        .collect()
}

#[test]
fn the_runners_lock_file_pins_what_the_workspace_does() {
    let workspace = pinned("Cargo.lock");
    let runner = pinned("crates/dibs-runner/provision/Cargo.lock");
    let strays: Vec<&String> = runner.difference(&workspace).collect();
    assert!(
        strays.is_empty(),
        "the runner's tree pins {strays:?}, which the workspace does not. Copy Cargo.lock into an \
         unpacked tree, run cargo metadata --offline there, and keep the lock file it leaves"
    );
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
/// by a holder, then puts the runner this suite runs where cargo would have built it.
const FAKE_CARGO: &str = r#"#!/bin/bash
[ "$*" = "build --locked --release" ] || { echo "cargo: not the build expected: $*" >&2; exit 2; }
[ -f Cargo.lock ] && [ -f install.sh ] && [ -f crates/dibs-runner/src/lib.rs ] || { echo "cargo: no runner tree here" >&2; exit 101; }
perl -MFcntl=:flock -e 'open(my $f, ">>", $ARGV[0]) or exit 0; exit(flock($f, LOCK_EX | LOCK_NB) ? 1 : 0)' "$DIBS_LOCK_DIR/rw" || { echo "cargo: built outside the lock" >&2; exit 101; }
grep -qs "dibs-runner" "$DIBS_LOCK_DIR"/holder.* || { echo "cargo: no holder names the build" >&2; exit 101; }
[ -z "${FAIL_BUILD:-}" ] || { echo "error: could not compile dibs-runner" >&2; exit 101; }
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
fn a_machine_with_no_runner_refuses_the_call_and_says_how_to_install_one() {
    let mut s = Sandbox::new();
    without_this_runner(&mut s, false);
    let out = s.remote(s.dibs(["--label", "never", "echo ran"])).run();
    assert_eq!(out.code, 72, "{}", out.all());
    assert_eq!(out.stderr.lines_with("dibs --check"), 1, "{}", out.stderr);
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
        s.records("waiting").iter().any(|r| r[3] == "dibs-runner")
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
fn the_machines_settings_file_comes_before_its_environment() {
    let s = Sandbox::new();
    s.write(
        "home/.config/dibs/machine.toml",
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
