//! The change notice: a session is told once when dibs changed under it.

use crate::caller::Caller;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

/// A stamp untouched this long belongs to a session that is gone.
const STAMP_LIFETIME: Duration = Duration::from_secs(31 * 86400);
const LISTED: usize = 10;

/// The clone this binary was built from.
pub fn clone_dir() -> PathBuf {
    let built = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    built.canonicalize().unwrap_or_else(|_| built.to_path_buf())
}

/// The commit the clone is at, or nothing outside a clone.
pub fn version() -> Option<String> {
    git(&clone_dir(), &["rev-parse", "--short", "HEAD"])
}

fn git(clone: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

pub struct ChangeNotice {
    pub seen: PathBuf,
}

impl ChangeNotice {
    /// Records this session's version, and says on stderr what changed since its last call.
    pub fn tell(&self, caller: &Caller) {
        let Some(now) = version() else {
            return;
        };
        let stamp = self.seen.join(caller.file_name());
        let was = std::fs::read_to_string(&stamp).unwrap_or_default();
        let was = was.trim_end();
        if was == now {
            return;
        }
        if std::fs::create_dir_all(&self.seen).is_ok() {
            let _ = std::fs::write(&stamp, format!("{now}\n"));
        }
        self.forget_old_sessions();
        if !was.is_empty() {
            eprint!("{}", ChangeNotice::text(&clone_dir(), was, &now));
        }
    }

    fn forget_old_sessions(&self) {
        let Ok(entries) = std::fs::read_dir(&self.seen) else {
            return;
        };
        let old = |meta: &std::fs::Metadata| {
            meta.modified()
                .ok()
                .and_then(|m| SystemTime::now().duration_since(m).ok())
                .is_some_and(|age| age >= STAMP_LIFETIME)
        };
        for entry in entries.flatten() {
            if entry.metadata().is_ok_and(|m| m.is_file() && old(&m)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn text(clone: &Path, was: &str, now: &str) -> String {
        let mut text = format!("dibs changed since this session last ran it: {was} -> {now}\n");
        let range = format!("{was}..{now}");
        let ancestor = Command::new("git")
            .arg("-C")
            .arg(clone)
            .args(["merge-base", "--is-ancestor", was, now])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ancestor {
            let count: usize = git(clone, &["rev-list", "--count", &range])
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let listed = format!("-{LISTED}");
            let log = git(
                clone,
                &["log", "--oneline", "--no-decorate", &listed, &range],
            )
            .unwrap_or_default();
            for line in log.lines() {
                text.push_str(&format!("  {line}\n"));
            }
            if count > LISTED {
                text.push_str(&format!(
                    "  and {} more:  git -C {} log {range}\n",
                    count - LISTED,
                    clone.display()
                ));
            }
        }
        text.push_str("  Flags and output you remember may be wrong now. Read dibs --help, and\n");
        text.push_str(&format!(
            "  {}/dibs-agent-rules.md for how it is meant to be used.\n",
            clone.display()
        ));
        text
    }
}
