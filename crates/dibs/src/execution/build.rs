//! What a build depends on: the signature its cargo command gives it, and its lockfile as the
//! short hashes a machine's seed and a target's record compare.

use dibs_format::lockfile::Package;
use sha2::{Digest, Sha256};

/// What a cargo build's artifacts depend on besides the lockfile: toolchain, profile, target,
/// feature flags and RUSTFLAGS. Two builds that differ here share almost nothing, so it is folded
/// into every line of the package list and they never match each other. `None` for anything
/// that is not a cargo command producing artifacts, `check` and `clippy` included, since those
/// leave metadata a build cannot use.
pub fn build_signature(run: &str) -> Option<String> {
    let unquote = |w: &str| w.trim_matches(|c| c == '"' || c == '\'').to_string();
    let run = run.replace("&&", "\n").replace("||", "\n");
    for segment in run.split([';', '|', '\n']) {
        let mut words = segment.split_whitespace().map(unquote).peekable();
        let mut rustflags = String::new();
        while let Some(w) = words.peek() {
            if w == "env" {
                words.next();
            } else if !w.starts_with('-') && w.contains('=') {
                if let Some(v) = w.strip_prefix("RUSTFLAGS=") {
                    rustflags = v.to_string();
                }
                words.next();
            } else {
                break;
            }
        }
        if words
            .next()
            .as_deref()
            .map(|p| p.rsplit('/').next() == Some("cargo"))
            != Some(true)
        {
            continue;
        }
        let mut toolchain = String::new();
        let mut sub = words.next().unwrap_or_default();
        if let Some(t) = sub.strip_prefix('+') {
            toolchain = t.to_string();
            sub = words.next().unwrap_or_default();
        }
        if !matches!(
            sub.as_str(),
            "build" | "b" | "test" | "t" | "bench" | "run" | "r" | "nextest"
        ) {
            continue;
        }
        let words: Vec<String> = words.collect();
        let mut profile = if sub == "bench" {
            "release".to_string()
        } else {
            "dev".to_string()
        };
        let (mut target, mut features, mut flags) = (String::new(), Vec::new(), Vec::new());
        let mut i = 0;
        while i < words.len() {
            let w = words[i].as_str();
            let next = words.get(i + 1).cloned().unwrap_or_default();
            match w {
                "--" => break,
                "--release" | "-r" => profile = "release".into(),
                "--profile" => profile = next,
                "--target" => target = next,
                "--features" | "-F" => features.extend(next.split(',').map(str::to_string)),
                "--no-default-features" | "--all-features" => flags.push(w.to_string()),
                _ => {
                    if let Some(v) = w.strip_prefix("--profile=") {
                        profile = v.into();
                    } else if let Some(v) = w.strip_prefix("--target=") {
                        target = v.into();
                    } else if let Some(v) = w
                        .strip_prefix("--features=")
                        .or_else(|| w.strip_prefix("-F"))
                    {
                        features.extend(v.split(',').map(str::to_string));
                    }
                }
            }
            i += 1;
        }
        features.retain(|f: &String| !f.is_empty());
        features.sort();
        features.dedup();
        flags.sort();
        flags.dedup();
        return Some(format!(
            "{toolchain}|{profile}|{target}|{}|{}|{rustflags}",
            flags.join(" "),
            features.join(",")
        ));
    }
    None
}

/// A lockfile as the short sorted hashes the machine's seed and a target's record compare. A git
/// package gets a line of its own, since a revision is what most often differs and costs most.
/// Registry packages share 64 lines, one per bucket of their contents, so a lockfile of a
/// thousand crates is still a short list.
pub fn packages(lock: &str, signature: &str) -> Vec<String> {
    let short = |text: &str| {
        format!(
            "{:.12}",
            hex(&Sha256::digest(format!("{signature}\n{text}").as_bytes()))
        )
    };
    let mut lines = std::collections::BTreeSet::new();
    let mut buckets: Vec<Vec<String>> = vec![Vec::new(); 64];
    let mut take = |name: Option<String>, version: Option<String>, source: Option<String>| {
        let (Some(n), Some(v), Some(src)) = (name, version, source) else {
            return;
        };
        let key = format!("{n} {v} {src}");
        if src.starts_with("git+") {
            lines.insert(short(&key));
        } else {
            let i = usize::from_str_radix(&short(&key)[..2], 16).unwrap_or(0) % 64;
            buckets[i].push(key);
        }
    };
    for package in Package::all(lock) {
        take(Some(package.name), package.version, package.source);
    }
    for (i, mut keys) in buckets.into_iter().enumerate() {
        if !keys.is_empty() {
            keys.sort();
            lines.insert(short(&format!("bucket {i}\n{}", keys.join("\n"))));
        }
    }
    lines.into_iter().collect()
}

/// Bytes as lowercase hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIG: &str = "|release||--no-default-features|cpu,fusion|";

    const LOCK_A: &str = "[[package]]\nname = \"cubecl\"\nversion = \"0.11.0\"\nsource = \"git+https://github.com/tracel-ai/cubecl?rev=aaa#aaa\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    fn packages_of(lock: &str) -> String {
        packages(lock, SIG)
            .iter()
            .map(|l| format!("{l}\n"))
            .collect()
    }

    #[test]
    fn a_lockfile_becomes_sorted_hashes_and_a_revision_is_a_different_package() {
        let a = packages_of(LOCK_A);
        assert_eq!(
            a.lines().count(),
            2,
            "one git package, one registry bucket, no workspace member"
        );
        let mut sorted: Vec<&str> = a.lines().collect();
        sorted.sort();
        assert_eq!(sorted, a.lines().collect::<Vec<_>>());
        let b = packages_of(&LOCK_A.replace("rev=aaa#aaa", "rev=bbb#bbb"));
        assert_eq!(a.lines().filter(|l| b.contains(*l)).count(), 1);
        let bumped = packages_of(&LOCK_A.replace("version = \"1.0.0\"", "version = \"1.0.1\""));
        assert_eq!(a.lines().filter(|l| bumped.contains(*l)).count(), 1);
        assert!(packages("", SIG).is_empty());
        let debug = packages(LOCK_A, "|dev||||").join("\n");
        assert_eq!(
            a.lines().filter(|l| debug.contains(*l)).count(),
            0,
            "another profile shares nothing"
        );
    }

    #[test]
    fn a_signature_is_what_a_build_depends_on_besides_the_lockfile() {
        let a = build_signature("start=$(date +%s); cargo build --release -p app --no-default-features --features cpu,fusion; rc=$?").unwrap();
        let b = build_signature(
            "cargo build -p app --features fusion,cpu --release --no-default-features",
        )
        .unwrap();
        assert_eq!(a, b, "flag order and feature order do not matter");
        assert_eq!(a, "|release||--no-default-features|cpu,fusion|");
        assert_ne!(
            a,
            build_signature("cargo build -p app --no-default-features --features cpu,fusion")
                .unwrap()
        );
        assert_eq!(
            build_signature("cargo bench --no-run").unwrap(),
            "|release||||"
        );
        assert_eq!(
            build_signature("cargo +nightly bench").unwrap(),
            "nightly|release||||"
        );
        assert_eq!(
            build_signature("cargo test --profile=ci -Fa --target x86_64-unknown-linux-gnu")
                .unwrap(),
            "|ci|x86_64-unknown-linux-gnu||a|"
        );
        assert_eq!(
            build_signature("RUSTFLAGS=-Ctarget-cpu=native ~/.cargo/bin/cargo build").unwrap(),
            "|dev||||-Ctarget-cpu=native"
        );
        assert_eq!(
            build_signature("cargo build && ./target/debug/app --features x --release").unwrap(),
            "|dev||||",
            "flags of a later command are not cargo's"
        );
        assert_eq!(
            build_signature("cargo run --release -- --features x").unwrap(),
            "|release||||",
            "arguments after -- go to the program"
        );
    }

    #[test]
    fn commands_that_leave_no_usable_artifacts_record_nothing() {
        for run in [
            "cargo fmt --check",
            "cargo clippy --release",
            "cargo check",
            "cargo --version",
            "./target/release/app bench",
        ] {
            assert_eq!(build_signature(run), None, "{run}");
        }
    }
}
