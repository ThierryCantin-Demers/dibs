//! The forced command every coworker's key runs: `dibs-orchestrator serve <coworker>`. Whatever
//! the client asked for is in `SSH_ORIGINAL_COMMAND`, and only two things are served: the API,
//! and an rsync into the coworker's own staging directory.

use crate::{
    api,
    home::{Home, component},
};
use dibs_format::{
    Exit,
    orchestrator::{API_COMMAND, STAGE_PREFIX},
};
use std::os::unix::{io::AsRawFd as _, process::CommandExt as _};

pub fn serve(home: &Home, coworker: &str) -> i32 {
    if !component(coworker) {
        eprintln!("dibs-orchestrator: {coworker:?} names no coworker");
        return Exit::Refused.status();
    }
    let asked = std::env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
    let words: Vec<&str> = asked.split_whitespace().collect();
    if asked.trim() == API_COMMAND {
        return api::serve(home, coworker);
    }
    match words.as_slice() {
        ["rsync", "--server", rest @ ..] => receive(home, coworker, rest),
        _ => {
            eprintln!(
                "dibs-orchestrator: this key serves `{API_COMMAND}` and uploads to {STAGE_PREFIX}, not {asked:?}"
            );
            Exit::Refused.status()
        }
    }
}

/// An rsync into `stage/<key>/` under the coworker's staging directory, against its last copy
/// of a repo, and nothing else: no download, no path outside it.
fn receive(home: &Home, coworker: &str, args: &[&str]) -> i32 {
    match Upload::check(args) {
        Ok(()) => {}
        Err(why) => {
            eprintln!("dibs-orchestrator: refused upload: {why}");
            return Exit::Refused.status();
        }
    }
    let staging = home.staging(coworker);
    if let Err(e) = std::fs::create_dir_all(staging.join(STAGE_PREFIX)) {
        eprintln!("dibs-orchestrator: {}: {e}", staging.display());
        return Exit::NoRoom.status();
    }
    // A link planted by an earlier upload under the same key would carry this one out of it.
    let dest = args
        .last()
        .copied()
        .unwrap_or_default()
        .trim_end_matches('/');
    let mut walked = staging.clone();
    for part in dest.split('/') {
        walked.push(part);
        if std::fs::symlink_metadata(&walked).is_ok_and(|m| m.file_type().is_symlink()) {
            eprintln!("dibs-orchestrator: refused upload: {dest} is a link");
            return Exit::Refused.status();
        }
    }
    let key = dest
        .strip_prefix(STAGE_PREFIX)
        .and_then(|rest| rest.split('/').find(|p| !p.is_empty()))
        .unwrap_or_default();
    let held = match home.stage_lock(coworker, key) {
        Ok(held) => held,
        Err(e) => {
            eprintln!("dibs-orchestrator: the stage's lock: {e}");
            return Exit::Failed.status();
        }
    };
    // Kept open across the exec, so rsync holds the stage until it exits.
    // SAFETY: clears close-on-exec on a descriptor this process owns.
    unsafe { libc::fcntl(held.as_raw_fd(), libc::F_SETFD, 0) };
    let error = std::process::Command::new("rsync")
        .arg("--server")
        .args(args)
        .current_dir(&staging)
        .exec();
    eprintln!("dibs-orchestrator: rsync could not start: {error}");
    Exit::Failed.status()
}

struct Upload;

impl Upload {
    const LONG: [&'static str; 4] = ["--partial", "--from0", "--checksum", "--no-times"];

    fn check(args: &[&str]) -> Result<(), String> {
        let Some((dest, options)) = args.split_last() else {
            return Err("no destination".into());
        };
        let Some((dot, options)) = options.split_last() else {
            return Err("no source placeholder".into());
        };
        if *dot != "." {
            return Err(format!("{dot:?} where rsync's server takes `.`"));
        }
        let mut options = options.iter();
        while let Some(option) = options.next() {
            let short = option.len() > 1
                && option.starts_with('-')
                && !option.starts_with("--")
                && option[1..]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.')
                // keep-dirlinks writes through a directory link in the destination.
                && !option[1..].contains('K');
            let allowed = short
                || Upload::LONG.contains(option)
                || option
                    .strip_prefix("--copy-dest=")
                    .is_some_and(Upload::last_copy)
                // rsync hands its server the path as a word of its own.
                || (*option == "--copy-dest"
                    && options.next().is_some_and(|path| Upload::last_copy(path)));
            if !allowed {
                return Err(format!("rsync's {option} is not taken here"));
            }
        }
        if Upload::stage_path(dest) {
            Ok(())
        } else {
            Err(format!("{dest} is not under {STAGE_PREFIX}<key>/"))
        }
    }

    /// `stage/<key>/` or its `.git/`.
    fn stage_path(dest: &str) -> bool {
        let Some(rest) = dest.strip_prefix(STAGE_PREFIX) else {
            return false;
        };
        let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
        match parts.as_slice() {
            [key] | [key, ".git"] => component(key),
            _ => false,
        }
    }

    /// `../../last/<repo>` or its `.git`, relative to a stage, as many levels up as it is deep.
    fn last_copy(path: &str) -> bool {
        let mut parts = path.split('/').peekable();
        let mut ups = 0;
        while parts.peek() == Some(&"..") {
            parts.next();
            ups += 1;
        }
        let rest: Vec<&str> = parts.collect();
        ups >= 2
            && match rest.as_slice() {
                ["last", repo] => component(repo),
                ["last", repo, ".git"] => component(repo),
                _ => false,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_goes_into_a_stage_and_nowhere_else() {
        let ok = |args: &[&str]| Upload::check(args).is_ok();
        assert!(ok(&["-logDtprze.iLsfxCIvu", "--partial", ".", "stage/k1/"]));
        assert!(ok(&[
            "-lpre.iLsfxCIvu",
            "--copy-dest=../../last/burn",
            ".",
            "stage/k1/"
        ]));
        assert!(ok(&[
            "-lpre.iLsfxCIvu",
            "--copy-dest=../../../last/burn/.git",
            ".",
            "stage/k1/.git/"
        ]));
        assert!(!ok(&["--sender", "-l", ".", "stage/k1/"]), "no download");
        assert!(!ok(&["-l", ".", "stage/../../x"]));
        assert!(!ok(&["-l", ".", "subs/x/"]));
        assert!(!ok(&["-l", ".", "stage/.k/"]));
        assert!(ok(&[
            "-lpre.iLsfxCIvu",
            "--copy-dest",
            "../../last/burn",
            ".",
            "stage/k1/"
        ]));
        assert!(!ok(&["--copy-dest", "/etc", ".", "stage/k/"]));
        assert!(!ok(&["--copy-dest=/etc", ".", "stage/k/"]));
        assert!(!ok(&["--copy-dest=../../last/../x", ".", "stage/k/"]));
        assert!(!ok(&["-l", "--rsync-path=sh", ".", "stage/k/"]));
        assert!(!ok(&["-lK", ".", "stage/k/"]), "no writing through a link");
        assert!(
            !ok(&["-l", ".", "stage/k/src/"]),
            "a stage or its .git only"
        );
    }
}
