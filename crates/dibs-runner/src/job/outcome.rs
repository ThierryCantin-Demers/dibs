use crate::clock::{Moment, Span};
use dibs_format::{JobId, JobMeta, wire::Built};
use std::{
    fs::{self, File},
    io::{self, BufRead as _, BufReader, Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// Lines past the head and tail a log may have before the digest leaves the middle out.
const DIGEST_SLACK: usize = 5;
/// What the tail of a log is first read back in.
const TAIL_CHUNK: u64 = 64 * 1024;

/// A job's log as its caller is shown it: enough to see how it went, never so much that the
/// caller has to cut it. Only its ends are read, however long it is.
pub struct Digest<'a> {
    pub path: &'a Path,
    /// Its newlines, as `LogRead` counted them.
    pub lines: usize,
    pub head: usize,
    pub tail: usize,
    pub host: &'a str,
    pub job: &'a JobId,
}

impl Digest<'_> {
    pub fn text(&self) -> Vec<u8> {
        if self.lines <= self.head + self.tail + DIGEST_SLACK {
            return fs::read(self.path).unwrap_or_default();
        }
        let Ok(mut file) = File::open(self.path) else {
            return Vec::new();
        };
        let mut text = Vec::new();
        let mut head = BufReader::new(&file);
        for _ in 0..self.head {
            if !matches!(head.read_until(b'\n', &mut text), Ok(1..)) {
                break;
            }
        }
        text.extend_from_slice(
            format!(
                "\n... {} lines omitted. The whole log:  dibs --on {} --out {}  ({})\n\n",
                self.lines - self.head - self.tail,
                self.host,
                self.job,
                self.path.display()
            )
            .as_bytes(),
        );
        text.extend(Digest::tail(&mut file, self.tail).unwrap_or_default());
        text
    }

    /// The last `n` lines, a last one without its newline counted as one, read back from the end
    /// in growing chunks until enough of them are in hand.
    fn tail(file: &mut File, n: usize) -> io::Result<Vec<u8>> {
        let len = file.metadata()?.len();
        let read_from = |file: &mut File, start: u64| -> io::Result<Vec<u8>> {
            file.seek(SeekFrom::Start(start))?;
            let mut chunk = Vec::new();
            file.take(len - start).read_to_end(&mut chunk)?;
            Ok(chunk)
        };
        let mut size = TAIL_CHUNK;
        let mut start = len.saturating_sub(size);
        let mut chunk = read_from(file, start)?;
        while start > 0 && chunk.iter().filter(|b| **b == b'\n').count() <= n {
            size *= 2;
            start = len.saturating_sub(size);
            chunk = read_from(file, start)?;
        }
        let pieces: Vec<&[u8]> = chunk.split_inclusive(|b| *b == b'\n').collect();
        let whole = &pieces[usize::from(start > 0).min(pieces.len())..];
        Ok(whole[whole.len().saturating_sub(n)..].concat())
    }
}

/// What a job's end needs of its log, read in one pass: its newlines, and what cargo built, read
/// from the log rather than the command, since what runs cargo may be a runner the command
/// starts. A finished cargo that compiled nothing means a measurement after it measures the
/// previous binary.
pub struct LogRead {
    pub lines: usize,
    pub built: Option<Built>,
}

impl LogRead {
    pub fn of(path: &Path) -> LogRead {
        let mut lines = 0;
        let (mut finished, mut compiled) = (false, 0u64);
        if let Ok(file) = File::open(path) {
            let mut log = BufReader::new(file);
            let mut line = Vec::new();
            while matches!(log.read_until(b'\n', &mut line), Ok(1..)) {
                lines += usize::from(line.last() == Some(&b'\n'));
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end_matches(['\n', '\r']).trim_start_matches(' ');
                finished |= text
                    .strip_prefix("Finished ")
                    .is_some_and(|rest| rest.contains("target(s) in "));
                // check and clippy print Checking for each crate they look at, and compile none.
                compiled +=
                    u64::from(text.starts_with("Compiling ") || text.starts_with("Checking "));
                line.clear();
            }
        }
        LogRead {
            lines,
            built: finished.then_some(match compiled {
                0 => Built::Nothing,
                n => Built::Crates(n),
            }),
        }
    }
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
        let path = std::env::temp_dir().join(format!("dibs-digest-{}", std::process::id()));
        let digest = |log: &str| {
            fs::write(&path, log).unwrap();
            String::from_utf8(
                Digest {
                    path: &path,
                    lines: LogRead::of(&path).lines,
                    head: 2,
                    tail: 2,
                    host: "box",
                    job: &job,
                }
                .text(),
            )
            .unwrap()
            .replace(&path.display().to_string(), "/s/jobs/j/log")
        };
        assert_eq!(digest("a\nb\nc\n"), "a\nb\nc\n");
        let long: String = (1..=12).map(|n| format!("{n}\n")).collect();
        assert_eq!(
            digest(&long),
            "1\n2\n\n... 8 lines omitted. The whole log:  dibs --on box --out j  (/s/jobs/j/log)\n\n11\n12\n"
        );
        let unended: String = (1..=12).map(|n| format!("{n}\n")).collect::<String>() + "13";
        assert!(
            digest(&unended).ends_with("\n\n12\n13"),
            "a last line without its newline"
        );
        let _ = fs::remove_file(&path);
    }

    fn read(log: &str) -> LogRead {
        let path =
            std::env::temp_dir().join(format!("dibs-logread-{}-{}", std::process::id(), log.len()));
        fs::write(&path, log).unwrap();
        let read = LogRead::of(&path);
        let _ = fs::remove_file(&path);
        read
    }

    #[test]
    fn what_cargo_built_is_counted_only_once_it_finished() {
        let log = "   Compiling a v1\n    Checking b v1\n    Finished `release` profile [optimized] target(s) in 3.2s\n";
        assert_eq!(
            (read(log).lines, read(log).built),
            (3, Some(Built::Crates(2)))
        );
        assert_eq!(
            read("    Finished `dev` profile target(s) in 0.1s\n").built,
            Some(Built::Nothing)
        );
        assert_eq!(read("   Compiling a v1\n").built, None);
    }
}
