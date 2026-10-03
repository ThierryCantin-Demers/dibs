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
