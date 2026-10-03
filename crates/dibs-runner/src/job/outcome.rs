use crate::clock::{Moment, Span};
use dibs_format::{JobId, JobMeta, wire::Built};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// Lines past the head and tail a log may have before the digest leaves the middle out.
const DIGEST_SLACK: usize = 5;

/// A job's log as its caller is shown it: enough to see how it went, never so much that the
/// caller has to cut it.
pub struct Digest<'a> {
    pub log: &'a [u8],
    pub head: usize,
    pub tail: usize,
    pub host: &'a str,
    pub job: &'a JobId,
    pub path: &'a Path,
}

impl Digest<'_> {
    pub fn text(&self) -> Vec<u8> {
        let lines: Vec<&[u8]> = self.log.split_inclusive(|b| *b == b'\n').collect();
        let count = self.log.iter().filter(|b| **b == b'\n').count();
        if count <= self.head + self.tail + DIGEST_SLACK {
            return self.log.to_vec();
        }
        let mut text: Vec<u8> = lines
            .iter()
            .take(self.head)
            .flat_map(|l| l.iter())
            .copied()
            .collect();
        text.extend_from_slice(
            format!(
                "\n... {} lines omitted. The whole log:  dibs --on {} --out {}  ({})\n\n",
                count - self.head - self.tail,
                self.host,
                self.job,
                self.path.display()
            )
            .as_bytes(),
        );
        let from = lines.len().saturating_sub(self.tail);
        text.extend(lines[from..].iter().flat_map(|l| l.iter()));
        text
    }
}

/// What cargo compiled, read from the log rather than the command, since what runs cargo may be
/// a runner the command starts. A finished cargo that compiled nothing means a measurement after
/// it measures the previous binary.
pub fn built(log: &[u8]) -> Option<Built> {
    let text = String::from_utf8_lossy(log);
    let starting = |line: &str, word: &str| line.trim_start_matches(' ').starts_with(word);
    let finished = text.lines().any(|l| {
        starting(l, "Finished ")
            && l.trim_start_matches(' ')["Finished ".len()..].contains("target(s) in ")
    });
    if !finished {
        return None;
    }
    let compiled = text.lines().filter(|l| starting(l, "Compiling ")).count() as u64;
    Some(match compiled {
        0 => Built::Nothing,
        n => Built::Crates(n),
    })
}

/// The same failing command, run again unchanged within the window, fails the same way.
pub struct Repeat<'a> {
    pub jobs: &'a Path,
    pub this: &'a Path,
    pub window: u64,
}

impl Repeat<'_> {
    /// The latest earlier failure of this exact command, as the trailer names it.
    pub fn earlier(&self) -> Option<String> {
        let command = fs::read(self.this.join("cmd")).ok()?;
        let now = SystemTime::now();
        let recent = Duration::from_secs((self.window / 60 + 1) * 60);
        let mut dirs: Vec<PathBuf> = fs::read_dir(self.jobs)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|d| d.is_dir() && d != self.this)
            .filter(|d| modified_within(d, now, recent))
            .collect();
        dirs.sort();
        dirs.iter()
            .filter_map(|dir| {
                let meta: JobMeta = fs::read_to_string(dir.join("meta")).ok()?.parse().ok()?;
                if fs::read(dir.join("cmd")).ok()? != command || meta.exit == 0 {
                    return None;
                }
                let wrote = fs::metadata(dir.join("meta")).ok()?.modified().ok()?;
                let age = now.duration_since(wrote).unwrap_or_default().as_secs();
                (age <= self.window).then(|| {
                    format!(
                        "{} exit {}, {} ago",
                        dir.file_name().unwrap_or_default().to_string_lossy(),
                        meta.exit,
                        Span(age)
                    )
                })
            })
            .next_back()
    }
}

fn modified_within(path: &Path, now: SystemTime, within: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|m| now.duration_since(m).unwrap_or_default() < within)
}

/// A job's id: when it arrived, in the machine's zone, and the pid of the runner that took it.
pub fn job_id(start: u64, pid: u32) -> JobId {
    JobId::new(format!("{}-{pid}", Moment::at(start).compact()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_log_is_shown_whole_and_a_long_one_by_its_ends() {
        let job = JobId::new("j");
        let digest = |log: &[u8]| {
            String::from_utf8(
                Digest {
                    log,
                    head: 2,
                    tail: 2,
                    host: "box",
                    job: &job,
                    path: Path::new("/s/jobs/j/log"),
                }
                .text(),
            )
            .unwrap()
        };
        assert_eq!(digest(b"a\nb\nc\n"), "a\nb\nc\n");
        let long: String = (1..=12).map(|n| format!("{n}\n")).collect();
        assert_eq!(
            digest(long.as_bytes()),
            "1\n2\n\n... 8 lines omitted. The whole log:  dibs --on box --out j  (/s/jobs/j/log)\n\n11\n12\n"
        );
    }

    #[test]
    fn what_cargo_built_is_counted_only_once_it_finished() {
        let log = b"   Compiling a v1\n   Compiling b v1\n    Finished `release` profile [optimized] target(s) in 3.2s\n";
        assert_eq!(built(log), Some(Built::Crates(2)));
        assert_eq!(
            built(b"    Finished `dev` profile target(s) in 0.1s\n"),
            Some(Built::Nothing)
        );
        assert_eq!(built(b"   Compiling a v1\n"), None);
    }
}
