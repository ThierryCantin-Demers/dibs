//! The runner's own tree, which machines build: it has to agree with the workspace it comes from.

use crate::harness::*;
use std::{collections::BTreeSet, fs};

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

/// A cargo that builds nothing: it checks it was run in the tree, then puts the runner this suite
/// runs where cargo would have built it.
const FAKE_CARGO: &str = r#"#!/bin/bash
[ "$*" = "build --locked --release" ] || { echo "cargo: not the build expected: $*" >&2; exit 2; }
[ -f Cargo.lock ] && [ -f install.sh ] && [ -f crates/dibs-runner/src/lib.rs ] || { echo "cargo: no runner tree here" >&2; exit 101; }
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
