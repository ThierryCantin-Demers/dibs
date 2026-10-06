//! The scratch tree a fixed sequence of prepares leaves on a machine: every directory, every
//! marker and what it holds. A prepare that moves into the runner has to leave the same tree,
//! or every build cache on every machine is rebuilt the day it ships.

use crate::harness::*;
use crate::recipes::{PARAMS, app, recipes};
use crate::snapshot::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub const LOCK: &str = "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"widget\"\nversion = \"0.2.0\"\nsource = \"git+https://example.invalid/widget?rev=abc#abc\"\n";

pub fn short_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .take(5)
        .collect()
}

/// Each path under `root` and what it is, with the contents of the files dibs writes itself.
pub fn tree(root: &Path, n: &Normal) -> Vec<String> {
    let entries: Vec<(String, PathBuf)> = fs::read_dir(root)
        .map(|d| {
            d.flatten()
                .map(|e| (n.apply(&e.file_name().to_string_lossy()), e.path()))
                .collect()
        })
        .unwrap_or_default();
    // Sorted by name, then as shown, so two files whose names normalise alike keep one order.
    let mut shown: Vec<(&String, Vec<String>)> = entries
        .iter()
        .map(|(name, p)| (name, entry(name, p, n)))
        .collect();
    shown.sort();
    shown.into_iter().flat_map(|(_, lines)| lines).collect()
}

fn entry(name: &str, p: &Path, n: &Normal) -> Vec<String> {
    let meta = fs::symlink_metadata(p).unwrap();
    if meta.is_dir() {
        if name == "jobs" {
            let n = fs::read_dir(p).map(|d| d.count()).unwrap_or(0);
            return vec![format!("{name}/  ({n} job directories)")];
        }
        let mut out = vec![format!("{name}/")];
        out.extend(tree(p, n).into_iter().map(|l| format!("  {l}")));
        return out;
    }
    if meta.file_type().is_symlink() {
        return vec![format!("{name} -> {}", fs::read_link(p).unwrap().display())];
    }
    if std::os::unix::fs::FileTypeExt::is_fifo(&meta.file_type()) {
        return vec![format!("{name}  (a fifo)")];
    }
    let text = fs::read_to_string(p).unwrap_or_default();
    let ours = (name.starts_with(".dibs-")
        || name.starts_with(".packages.")
        || matches!(name, ".prepare.lock" | "config.toml" | ".git"))
        && !name.ends_with(".toml.lock");
    match (ours, text.is_empty()) {
        (true, true) => vec![format!("{name}  (empty)")],
        (true, false) => std::iter::once(format!("{name}:"))
            .chain(text.lines().map(|l| format!("  | {l}")))
            .collect(),
        (false, _) => vec![name.to_string()],
    }
}

#[test]
fn the_scratch_a_fixed_sequence_of_prepares_leaves() {
    let mut s = Sandbox::new();
    // Whether this filesystem shares blocks would otherwise decide which trees get seeded.
    s.set("DIBS_REFLINK", "never");
    let dir = app(&s);
    s.write_exec(
        "home/.cargo/bin/cargo",
        "#!/bin/bash\necho \"    Finished \\`release\\` profile [optimized] target(s) in 0.01s\"\n",
    );
    s.write("app/Cargo.lock", LOCK);
    recipes(
        &s,
        &format!(
            "{PARAMS}\n[bench.gate]\n  [[bench.gate.step]]\n  lock = \"shared\"\n  run = \"cargo build --release\"\n  [[bench.gate.step]]\n  lock = \"exclusive\"\n  run = \"echo measured\"\n"
        ),
    );
    s.git("app", &["add", "-A"]);
    s.git("app", &["commit", "-qm", "recipes and a lockfile"]);
    s.git("app", &["push", "-q", "origin", "HEAD:main"]);
    let main = s.git("app", &["rev-parse", "HEAD"]);
    s.git("app", &["worktree", "add", "-q", &s.p("app-topk")]);
    s.write("app/a.txt", "edited here\n");

    let mut t = Transcript::default();
    let steps: Vec<(&str, Vec<String>)> = vec![
        (
            "a ref, fetched",
            vec!["build".into(), format!("{dir}@main"), "p".into()],
        ),
        (
            "a measurement at that ref, which builds",
            vec!["bench".into(), format!("{dir}@main"), "gate".into()],
        ),
        (
            "the working tree",
            vec!["build".into(), format!("{dir}@local"), "p".into()],
        ),
        (
            "the working tree, measured",
            vec!["bench".into(), format!("{dir}@local"), "gate".into()],
        ),
        (
            "a second checkout of the repo",
            vec![
                "bench".into(),
                format!("{}@local", s.p("app-topk")),
                "gate".into(),
            ],
        ),
        (
            "a comparison of main and the working tree",
            vec![
                "bench".into(),
                format!("{dir}@origin/main..local"),
                "gate".into(),
            ],
        ),
    ];
    let mut n = Normal::of(&s);
    let names = [(dir.clone(), "app"), (s.p("app-topk"), "app-topk")];
    for (path, name) in &names {
        n = n.literal(
            &format!("local-{}", short_hash(path)),
            &format!("local-<{name}>"),
        );
    }
    n = n.literal(
        &format!("local-{}", short_hash(&format!("base\0app\0{main}"))),
        "local-<main, sent>",
    );
    n = n
        .literal(&main[..12], "<main>")
        .rule(r"\b[0-9]+-[0-9]{16,}\b", "<token>")
        .rule(r"(?m)^(branch refs/heads/)\S+$", "$1<the default branch>");
    for (what, args) in &steps {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = s.dibs(&args).run();
        assert_eq!(out.code, 0, "{what}: {}", out.all());
        t.section(
            &format!("after {what}: dibs {}", n.apply(&typed(&args))),
            &n.apply(&format!("{}\n", tree(&s.path("scratch"), &n).join("\n"))),
        );
    }
    let worktrees = s.git("home/prog/app", &["worktree", "list", "--porcelain"]);
    let worktrees: Vec<String> = worktrees
        .lines()
        .filter(|l| !l.starts_with("HEAD "))
        .map(str::to_string)
        .collect();
    t.section(
        "the clone's own record of its worktrees",
        &n.apply(&format!("{}\n", worktrees.join("\n"))),
    );
    t.section(
        "refs a prepare left under refs/dibs",
        &format!(
            "{}\n",
            s.git("home/prog/app", &["for-each-ref", "refs/dibs"])
                .lines()
                .count()
        ),
    );
    snapshot("prepares", t.text());
}
