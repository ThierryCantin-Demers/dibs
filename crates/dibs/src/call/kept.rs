use crate::{
    call::{
        base::CallError,
        machine::{Asked, MachineCall},
    },
    cli::OutTarget,
    machine::Target,
    render::{Indented, KeptLog},
};
use dibs_format::{Exit, JobId, Label, Mode, base64};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const OUT_LINES: &str = "40";
/// The first line of a finished job's whole log, as the machine sends it to be kept.
const OUT_HEAD: &str = "DIBS-OUT-HEAD";
/// The line between that heading and the log itself.
const OUT_LOG: &str = "DIBS-OUT-LOG";
/// The first line of a job's files, sent as a base64 tar.
const FETCHED: &str = "DIBS-FETCH";
const FILES_LISTED: usize = 20;

impl MachineCall<'_> {
    /// `dibs --out`: what the running jobs are writing, or one job's log, kept here once it is
    /// finished so it can be read after its machine is gone.
    pub fn out(&self, target: Option<&OutTarget>) -> Result<i32, CallError> {
        let lines = std::env::var("DIBS_OUT_LINES")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| OUT_LINES.into());
        let shown = match target {
            Some(OutTarget::Job(job)) => return KeptJob::of(self, job).out(&lines),
            Some(OutTarget::Pid(pid)) => pid.to_string(),
            None => "all".into(),
        };
        let at = self.target()?;
        self.somewhere(&at)?;
        let label = Label::new(format!("{shown}.{lines}"));
        self.send(Asked::plain(Mode::Out, label), &at)
    }

    /// `dibs --fetch`: a job's files, kept here and copied where asked.
    pub fn fetch(
        &self,
        job: &JobId,
        into: Option<&str>,
        report: &mut dyn Write,
    ) -> Result<i32, CallError> {
        KeptJob::of(self, job).fetch(into, report)
    }
}

/// A job's log and files as this computer keeps them, fetched from its machine the first time
/// they are asked for, so they can be read after the machine is gone.
struct KeptJob<'a> {
    machine: &'a MachineCall<'a>,
    job: &'a JobId,
    dir: PathBuf,
}

impl<'a> KeptJob<'a> {
    fn of(machine: &'a MachineCall<'a>, job: &'a JobId) -> KeptJob<'a> {
        let dir = machine
            .paths
            .kept_jobs()
            .unwrap_or_default()
            .join(job.as_str());
        KeptJob { machine, job, dir }
    }

    fn out(&self, lines: &str) -> Result<i32, CallError> {
        let (machine, kept) = (self.machine, &self.dir);
        let at = machine.target()?;
        if !kept.join("log").is_file() {
            machine.somewhere(&at)?;
            if let Some(exit) = self.keep_log(&at)? {
                return Ok(exit);
            }
        }
        let text = fs::read(kept.join("log"))?;
        let log = kept.join("log");
        print!(
            "{}",
            KeptLog {
                head: &fs::read_to_string(kept.join("head")).unwrap_or_default(),
                log: &log,
                bytes: text.len() as u64,
                text: &String::from_utf8_lossy(&text),
                lines,
                last: lines.parse().unwrap_or_default(),
            }
        );
        Ok(0)
    }

    /// Fetches the finished job's whole log; the exit to give instead, when the machine sent
    /// something else.
    fn keep_log(&self, at: &Target) -> Result<Option<i32>, CallError> {
        let (job, kept) = (self.job, &self.dir);
        let label = Label::new(format!("{job}.whole"));
        let answer = self.machine.capture(Asked::plain(Mode::Out, label), at)?;
        let exit = answer.exit.unwrap_or_default();
        let raw = answer.output;
        let part = kept.with_extension("part");
        let Some(whole) = WholeLog::read(&raw).filter(|_| fs::create_dir_all(&part).is_ok()) else {
            io::stdout().write_all(&raw)?;
            return Ok(Some(exit));
        };
        fs::write(part.join("head"), whole.head)?;
        fs::write(part.join("log"), whole.log)?;
        fs::create_dir_all(kept)?;
        fs::rename(part.join("head"), kept.join("head"))?;
        fs::rename(part.join("log"), kept.join("log"))?;
        let _ = fs::remove_dir_all(&part);
        Ok(None)
    }

    fn fetch(&self, into: Option<&str>, report: &mut dyn Write) -> Result<i32, CallError> {
        let (machine, job) = (self.machine, self.job);
        let kept = self.dir.join("artifacts");
        if !kept.is_dir() {
            let at = machine.target()?;
            machine.somewhere(&at)?;
            let answer =
                machine.capture(Asked::plain(Mode::Fetch, Label::new(job.as_str())), &at)?;
            let raw = answer.output;
            let Some(tar) = raw
                .strip_prefix(FETCHED.as_bytes())
                .and_then(|r| r.strip_prefix(b"\n"))
            else {
                report.write_all(&raw)?;
                return Ok(match answer.exit.unwrap_or_default() {
                    0 => i32::from(Exit::Failed.code()),
                    exit => exit,
                });
            };
            if !unpack(tar, &kept) {
                eprintln!(
                    "dibs: the files of job {job} arrived but could not be unpacked into {}",
                    kept.display()
                );
                return Ok(i32::from(Exit::Failed.code()));
            }
        }
        let files = files_under(&kept);
        writeln!(
            report,
            "dibs: {} file(s) from job {job}, kept in {}",
            files.len(),
            kept.display()
        )?;
        let listed: Vec<String> = files.iter().take(FILES_LISTED).cloned().collect();
        write!(
            report,
            "{}",
            Indented {
                text: &listed.join("\n"),
                by: "  ",
            }
        )?;
        if files.len() > FILES_LISTED {
            writeln!(report, "  and {} more", files.len() - FILES_LISTED)?;
        }
        if let Some(into) = into {
            let copied = fs::create_dir_all(into).is_ok()
                && Command::new("cp")
                    .arg("-a")
                    .arg(format!("{}/.", kept.display()))
                    .arg(format!("{into}/"))
                    .status()
                    .is_ok_and(|s| s.success());
            if !copied {
                return Ok(i32::from(Exit::Failed.code()));
            }
            writeln!(report, "dibs: copied into {into}")?;
        }
        Ok(0)
    }
}

/// A finished job's log as the machine sends it to be kept: a heading, then the log.
struct WholeLog<'a> {
    head: &'a [u8],
    log: &'a [u8],
}

impl<'a> WholeLog<'a> {
    fn read(raw: &'a [u8]) -> Option<WholeLog<'a>> {
        let rest = raw.strip_prefix(OUT_HEAD.as_bytes())?.strip_prefix(b"\n")?;
        let marker = format!("{OUT_LOG}\n");
        let at = match rest.starts_with(marker.as_bytes()) {
            true => 0,
            false => {
                let inner = format!("\n{marker}");
                rest.windows(inner.len())
                    .position(|w| w == inner.as_bytes())?
                    + 1
            }
        };
        Some(WholeLog {
            head: &rest[..at],
            log: &rest[at + marker.len()..],
        })
    }
}

/// Unpacks a base64 tar into `into`, through a sibling directory so a failure leaves nothing.
fn unpack(tar: &[u8], into: &Path) -> bool {
    let part = into.with_extension("part");
    let _ = fs::remove_dir_all(&part);
    let unpacked = base64::decode(tar)
        .is_some_and(|bytes| fs::create_dir_all(&part).is_ok() && untar(&bytes, &part))
        && fs::rename(&part, into).is_ok();
    if !unpacked {
        let _ = fs::remove_dir_all(&part);
    }
    unpacked
}

fn untar(bytes: &[u8], into: &Path) -> bool {
    let Ok(mut tar) = Command::new("tar")
        .arg("-C")
        .arg(into)
        .args(["-xf", "-"])
        .stdin(Stdio::piped())
        .spawn()
    else {
        return false;
    };
    let written = tar
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(bytes).is_ok());
    tar.wait().is_ok_and(|s| s.success()) && written
}

/// Every file under a directory, by its path inside it, sorted.
fn files_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).into_iter().flatten().flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(path),
                Ok(kind) if kind.is_file() => {
                    if let Ok(inside) = path.strip_prefix(dir) {
                        found.push(inside.display().to_string());
                    }
                }
                _ => {}
            }
        }
    }
    found.sort();
    found
}
