//! What a job left on the machine, read back without the lock: `dibs --out` and `dibs --fetch`.

use crate::{
    clock::{Moment, Span},
    job::Tree,
    lock::{Kind, LockDir},
    machine::Site,
    settings::var,
    sink::Sink,
};
use dibs_format::{Exit, JobMeta, base64};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Lines of a log shown when the caller asked for its whole.
const WHOLE_SHOWN: usize = 40;
/// The most of a log or of a job's files sent with no lock, beside whatever is measured.
const WHOLE_MAX: u64 = 16 * 1024 * 1024;
const FILES_MAX: u64 = 64 * 1024 * 1024;
const COMMAND_SHOWN: usize = 200;
const OUT_HEAD: &str = "DIBS-OUT-HEAD";
const OUT_LOG: &str = "DIBS-OUT-LOG";
const FETCHED: &str = "DIBS-FETCH";

/// The jobs a machine keeps, and where what is read of them goes.
pub struct KeptJobs<'a> {
    pub machine: &'a Site,
    pub dir: &'a LockDir,
    pub sink: &'a Sink,
    /// The caller's stdout is a terminal, so a hint may be dimmed.
    pub tty: bool,
}

/// What `--out` was asked for: `<job id, pid or all>.<lines or whole>`.
struct Asked<'a> {
    target: &'a str,
    lines: usize,
    whole: bool,
}

impl KeptJobs<'_> {
    pub fn out(&self, label: &str) -> i32 {
        let target = label.split('.').next().unwrap_or_default();
        let last = label.rsplit('.').next().unwrap_or_default();
        let asked = Asked {
            target,
            lines: last.parse().unwrap_or(WHOLE_SHOWN),
            whole: last == "whole",
        };
        match target.contains('-') {
            true => self.job(&asked),
            false => self.holders(&asked),
        }
    }

    /// A job by its id, running or finished; the whole of a finished one's log for the caller to
    /// keep, when that is what it asked for.
    fn job(&self, asked: &Asked) -> i32 {
        let jobs = self.machine.jobs();
        let dir = jobs.join(asked.target);
        let log = dir.join("log");
        if !log.is_file() {
            self.sink.say(&format!(
                "no job {} under {}\n",
                asked.target,
                jobs.display()
            ));
            return Exit::Failed.status();
        }
        let meta = fs::read_to_string(dir.join("meta"))
            .ok()
            .and_then(|m| m.parse::<JobMeta>().ok());
        let ended = meta.as_ref().map(|m| {
            format!(
                "job {}  {}  {}  ran {}s  exit {}",
                asked.target, m.mode, m.label, m.ran, m.exit
            )
        });
        let command = fs::read(dir.join("cmd")).unwrap_or_default();
        let command = String::from_utf8_lossy(&command[..command.len().min(COMMAND_SHOWN)])
            .replace('\n', " ");
        let bytes = fs::metadata(&log).map_or(0, |m| m.len());
        let text = fs::read(&log).unwrap_or_default();
        if let Some(ended) = ended.as_ref().filter(|_| asked.whole && bytes <= WHOLE_MAX) {
            self.sink.out(
                format!(
                    "{OUT_HEAD}\n{ended}\n  {command}\n  {}:{}\n{OUT_LOG}\n",
                    self.machine.host,
                    log.display()
                )
                .as_bytes(),
            );
            self.sink.out(&text);
            return 0;
        }
        let heading = ended.unwrap_or_else(|| format!("job {}  still running", asked.target));
        self.sink.out(
            format!(
                "{heading}\n  {command}\n  {}  ({bytes} bytes, last {} lines)\n",
                log.display(),
                asked.lines
            )
            .as_bytes(),
        );
        self.sink.out(&tail(&text, asked.lines));
        0
    }

    /// What the holders of the lock are writing: a file their tree writes to, or their log.
    fn holders(&self, asked: &Asked) -> i32 {
        let mut found = false;
        for holder in self.dir.records(Kind::Holder) {
            if asked.target != "all" && asked.target != holder.pid.to_string() {
                continue;
            }
            found = true;
            let age = Span(Moment::epoch_now().saturating_sub(holder.start));
            let mut said = format!(
                "{}  {}  pid {}  ({age})\n  from {}\n",
                holder.mode, holder.label, holder.pid, holder.agent
            );
            let mut files = Tree::of(holder.pid).written();
            if files.is_empty()
                && let Some(log) = self.log_of(holder.pid)
            {
                files.push(log);
            }
            if files.is_empty() {
                let (dim, off) = match self.tty && var("NO_COLOR").is_none() {
                    true => ("\x1b[2m", "\x1b[0m"),
                    false => ("", ""),
                };
                said.push_str(&format!(
                    "  writing straight back to the agent that started it, so there is no\n  \
                     copy on disk to show. Only a job that redirects into a file, which is\n  \
                     {dim}cmd > $DIBS_SCRATCH/x.log 2>&1, can be read from here.{off}\n"
                ));
            }
            self.sink.out(said.as_bytes());
            for file in files {
                let text = fs::read(&file).unwrap_or_default();
                self.sink.out(
                    format!(
                        "\n  {}  ({} bytes, last {} lines)\n",
                        file.display(),
                        text.len(),
                        asked.lines
                    )
                    .as_bytes(),
                );
                self.sink.out(&tail(&text, asked.lines));
            }
        }
        match (found, asked.target) {
            (true, _) => 0,
            (false, "all") => {
                self.sink.out(b"Nothing is running.\n");
                0
            }
            (false, pid) => {
                self.sink
                    .say(&format!("Nothing holding the lock with pid {pid}.\n"));
                1
            }
        }
    }

    /// The log of the job a holder runs.
    fn log_of(&self, pid: u32) -> Option<PathBuf> {
        let ending = format!("-{pid}");
        let mut jobs: Vec<PathBuf> = fs::read_dir(self.machine.jobs())
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(&ending))
            .map(|e| e.path().join("log"))
            .filter(|log| log.exists())
            .collect();
        jobs.sort();
        jobs.into_iter().next()
    }

    /// A job's kept files as a base64 tar, capped, since no lock is taken to send them.
    pub fn fetch(&self, job: &str) -> i32 {
        let jobs = self.machine.jobs();
        let dir = jobs.join(job);
        if !dir.is_dir() {
            self.sink
                .say(&format!("no job {job} under {}\n", jobs.display()));
            return Exit::Failed.status();
        }
        let files = dir.join("artifacts");
        if !files.is_dir() {
            self.sink.say(&format!(
                "job {job} kept no files: its recipe names no artifacts, or it wrote none of them\n"
            ));
            return Exit::Setup.status();
        }
        let size = apparent_size(&files);
        if size > FILES_MAX {
            self.sink.say(&format!(
                "job {job} kept {size} bytes, more than a fetch without a lock takes. Copy them under the shared lock:\n  \
                 dibs --sync -a :{}/ ./\n",
                files.display()
            ));
            return Exit::Refused.status();
        }
        let tar = Command::new("tar")
            .arg("-C")
            .arg(&files)
            .args(["-cf", "-", "."])
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output();
        match tar {
            Ok(tar) if tar.status.success() => {
                self.sink.out(format!("{FETCHED}\n").as_bytes());
                self.sink.out(base64::encode_lines(&tar.stdout).as_bytes());
                0
            }
            _ => {
                self.sink.say(&format!(
                    "dibs: the files of job {job} could not be packed\n"
                ));
                1
            }
        }
    }
}

/// The last lines of a text, each shown under a bar.
fn tail(text: &[u8], lines: usize) -> Vec<u8> {
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    if body.is_empty() || lines == 0 {
        return Vec::new();
    }
    let all: Vec<&[u8]> = body.split(|b| *b == b'\n').collect();
    let mut out = Vec::new();
    for line in &all[all.len().saturating_sub(lines)..] {
        out.extend_from_slice(b"  | ");
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

/// What `du -sb` counts: every file's and directory's own size.
fn apparent_size(dir: &Path) -> u64 {
    let own = fs::symlink_metadata(dir).map_or(0, |m| m.len());
    let below: u64 = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => apparent_size(&entry.path()),
            _ => entry.metadata().map_or(0, |m| m.len()),
        })
        .sum();
    own + below
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_shows_the_last_lines_under_a_bar() {
        assert_eq!(tail(b"a\nb\nc\n", 2), b"  | b\n  | c\n");
        assert_eq!(tail(b"a\nb", 5), b"  | a\n  | b\n");
        assert!(tail(b"", 3).is_empty());
    }
}
