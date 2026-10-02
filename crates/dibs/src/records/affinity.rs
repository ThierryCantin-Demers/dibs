use crate::records::runs::now_secs;
use dibs::paths::Paths;
use std::path::PathBuf;

/// Which machine holds a repo's build cache. Kept beside the run record, on this side, since
/// it describes the pool rather than any one machine in it.
pub(crate) fn affinity_path() -> Option<PathBuf> {
    Paths::from_env().affinity()
}

/// The machine deletes a target directory unused this long, so a memo of one is kept no longer.
pub(crate) const AFFINITY_SECS: u64 = 5 * 86400;

/// Lines older than the cache they name, and lines without a time, are not read.
pub(crate) fn affinity_live(text: &str, now: u64) -> impl Iterator<Item = (&str, &str, u64)> {
    text.lines().filter_map(move |l| {
        let mut f = l.split('\t');
        let (repo, machine, used) = (f.next()?, f.next()?, f.next()?.trim().parse::<u64>().ok()?);
        (now.saturating_sub(used) < AFFINITY_SECS).then_some((repo, machine, used))
    })
}

pub(crate) fn affinity_lookup(text: &str, repo: &str, now: u64) -> Option<String> {
    affinity_live(text, now)
        .find(|(r, ..)| *r == repo)
        .map(|(_, m, _)| m.to_string())
}

pub(crate) fn affinity_update(text: &str, repo: &str, machine: &str, now: u64) -> String {
    let mut lines: Vec<String> = affinity_live(text, now)
        .filter(|(r, ..)| *r != repo)
        .map(|(r, m, t)| format!("{r}\t{m}\t{t}"))
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
    let text = affinity_update(
        &std::fs::read_to_string(&p).unwrap_or_default(),
        repo,
        machine,
        now_secs(),
    );
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension(std::process::id().to_string());
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// `--on` reaches here as DIBS_ON. A call sent to a named machine is not ranked, and says
/// nothing about where the repo's cache belongs.
pub(crate) fn pinned() -> bool {
    std::env::var("DIBS_ON").is_ok_and(|m| !m.is_empty())
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
