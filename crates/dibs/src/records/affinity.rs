use crate::{paths::Paths, records::runs::now_secs};
use dibs_format::Span;
use dibs_runner::shared::SharedFile;
use std::path::PathBuf;

/// The machine deletes a target directory unused this long, so a memo of one is kept no longer.
const AFFINITY_SECS: u64 = 5 * Span::DAY.0;

/// Which machine holds each repo's build cache. Kept beside the run record, on this side, since
/// it describes the pool rather than any one machine in it.
pub struct Affinity {
    path: Option<PathBuf>,
}

impl Affinity {
    /// This computer's, where the environment puts it.
    pub fn here() -> Affinity {
        Affinity {
            path: Paths::from_env().affinity(),
        }
    }

    /// The machine holding `repo`'s build cache, while its cache can still be there.
    pub fn get(&self, repo: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.path.as_ref()?).ok()?;
        Claims(&text).holder(repo, now_secs())
    }

    /// `machine` holds `repo`'s build cache from now on.
    pub fn set(&self, repo: &str, machine: &str) {
        let Some(path) = &self.path else { return };
        let _ =
            SharedFile { path }.rewrite(|text| Some(Claims(text).with(repo, machine, now_secs())));
    }
}

/// One line of the affinity file: the machine holding a repo's build cache, and when it was last
/// used there.
struct Claim<'a> {
    repo: &'a str,
    machine: &'a str,
    used: u64,
}

/// The affinity file's text.
struct Claims<'a>(&'a str);

impl<'a> Claims<'a> {
    /// Lines older than the cache they name, and lines without a time, are not read.
    fn live(&self, now: u64) -> impl Iterator<Item = Claim<'a>> {
        self.0.lines().filter_map(move |l| {
            let mut f = l.split('\t');
            let claim = Claim {
                repo: f.next()?,
                machine: f.next()?,
                used: f.next()?.trim().parse::<u64>().ok()?,
            };
            (now.saturating_sub(claim.used) < AFFINITY_SECS).then_some(claim)
        })
    }

    fn holder(&self, repo: &str, now: u64) -> Option<String> {
        self.live(now)
            .find(|c| c.repo == repo)
            .map(|c| c.machine.to_string())
    }

    fn with(&self, repo: &str, machine: &str, now: u64) -> String {
        let mut lines: Vec<String> = self
            .live(now)
            .filter(|c| c.repo != repo)
            .map(|c| format!("{}\t{}\t{}", c.repo, c.machine, c.used))
            .collect();
        lines.push(format!("{repo}\t{machine}\t{now}"));
        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affinity_is_one_machine_per_repo_and_expires_with_the_cache() {
        let day = Span::DAY.0;
        let text = Claims("cubek\tbox-a\n").with("cubek", "box-b", 10 * day);
        let text = Claims(&text).with("burn", "box-a", 12 * day);
        assert_eq!(
            text.lines().count(),
            2,
            "the untimed line is dropped: {text}"
        );
        assert_eq!(
            Claims(&text).holder("cubek", 14 * day).as_deref(),
            Some("box-b")
        );
        assert_eq!(Claims(&text).holder("cubek", 15 * day), None);
        assert_eq!(
            Claims(&text).holder("burn", 15 * day).as_deref(),
            Some("box-a")
        );
        let text = Claims(&text).with("burn", "box-b", 16 * day);
        assert_eq!(text, format!("burn\tbox-b\t{}\n", 16 * day));
    }
}
