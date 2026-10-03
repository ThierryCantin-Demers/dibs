use crate::records::runs::now_secs;
use dibs::{cli::RecipeCall, paths::Paths};
use dibs_runner::shared::SharedFile;
use std::path::PathBuf;

/// Which machine holds a repo's build cache. Kept beside the run record, on this side, since
/// it describes the pool rather than any one machine in it.
pub(crate) fn affinity_path() -> Option<PathBuf> {
    Paths::from_env().affinity()
}

/// The machine deletes a target directory unused this long, so a memo of one is kept no longer.
pub(crate) const AFFINITY_SECS: u64 = 5 * 86400;

/// One line of the affinity file: the machine holding a repo's build cache, and when it was last
/// used there.
struct Claim<'a> {
    repo: &'a str,
    machine: &'a str,
    used: u64,
}

/// Lines older than the cache they name, and lines without a time, are not read.
fn affinity_live(text: &str, now: u64) -> impl Iterator<Item = Claim<'_>> {
    text.lines().filter_map(move |l| {
        let mut f = l.split('\t');
        let claim = Claim {
            repo: f.next()?,
            machine: f.next()?,
            used: f.next()?.trim().parse::<u64>().ok()?,
        };
        (now.saturating_sub(claim.used) < AFFINITY_SECS).then_some(claim)
    })
}

pub(crate) fn affinity_lookup(text: &str, repo: &str, now: u64) -> Option<String> {
    affinity_live(text, now)
        .find(|c| c.repo == repo)
        .map(|c| c.machine.to_string())
}

pub(crate) fn affinity_update(text: &str, repo: &str, machine: &str, now: u64) -> String {
    let mut lines: Vec<String> = affinity_live(text, now)
        .filter(|c| c.repo != repo)
        .map(|c| format!("{}\t{}\t{}", c.repo, c.machine, c.used))
        .collect();
    lines.push(format!("{repo}\t{machine}\t{now}"));
    lines.join("\n") + "\n"
}

pub(crate) fn affinity_get(repo: &str) -> Option<String> {
    affinity_lookup(
        &std::fs::read_to_string(affinity_path()?).ok()?,
        repo,
        now_secs(),
    )
}

pub(crate) fn affinity_set(repo: &str, machine: &str) {
    let Some(p) = affinity_path() else { return };
    let _ = SharedFile { path: &p }
        .rewrite(|text| Some(affinity_update(text, repo, machine, now_secs())));
}

/// A call sent to a named machine, by `--on` or `DIBS_ON`, is not ranked, and says nothing
/// about where the repo's cache belongs.
pub(crate) fn pinned(args: &RecipeCall) -> bool {
    args.on.is_some() || std::env::var("DIBS_ON").is_ok_and(|m| !m.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affinity_is_one_machine_per_repo_and_expires_with_the_cache() {
        let day = 86400;
        let text = affinity_update("cubek\tbox-a\n", "cubek", "box-b", 10 * day);
        let text = affinity_update(&text, "burn", "box-a", 12 * day);
        assert_eq!(
            text.lines().count(),
            2,
            "the untimed line is dropped: {text}"
        );
        assert_eq!(
            affinity_lookup(&text, "cubek", 14 * day).as_deref(),
            Some("box-b")
        );
        assert_eq!(affinity_lookup(&text, "cubek", 15 * day), None);
        assert_eq!(
            affinity_lookup(&text, "burn", 15 * day).as_deref(),
            Some("box-a")
        );
        let text = affinity_update(&text, "burn", "box-b", 16 * day);
        assert_eq!(text, format!("burn\tbox-b\t{}\n", 16 * day));
    }
}
