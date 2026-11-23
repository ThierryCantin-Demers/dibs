//! A job's call lets go of it once it has run as long as `--check-in` gives it, so the agent that
//! started it hears of it, while the job goes on; `dibs --follow` hears each check-in after.

use crate::{ahead::Frozen, kept::tail, stop::Stage, stop::Stopper};
use dibs_format::{JobId, Span, wire::CheckedIn};
use std::{
    fs::{self, File},
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        mpsc::{self, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const TICK: Duration = Duration::from_secs(1);
const LOG_LINES: usize = 10;
const SERVICE_LINES: usize = 5;
/// The most of a log's end read for its last lines, beside whatever is measured.
const TAIL_BYTES: u64 = 64 * 1024;

/// Held from the hand-over until this process exits: a follower takes it to learn the job ended.
static HANDED: OnceLock<File> = OnceLock::new();

/// Where a job's check-ins are kept, beside its log.
pub struct CheckIns<'a> {
    pub dir: &'a Path,
}

impl CheckIns<'_> {
    /// Locked for as long as the runner that let go of the job lives.
    pub fn handed(&self) -> PathBuf {
        self.dir.join("handed")
    }

    /// What the call says once it has let go: the digest, the trailer and the exit.
    pub fn report(&self) -> PathBuf {
        self.dir.join("report")
    }

    /// The latest check-in: its number, the runner's pid and the seconds between check-ins on
    /// the first line, then its text.
    fn latest(&self) -> PathBuf {
        self.dir.join("check-in")
    }

    fn write_latest(&self, n: u32, checked: &CheckedIn, text: &str) {
        let part = self.latest().with_extension("part");
        let first = format!("{n}\t{}\t{}", checked.pid, checked.every);
        if fs::write(&part, format!("{first}\n{text}")).is_ok() {
            let _ = fs::rename(&part, self.latest());
        }
    }

    /// The number of the latest check-in, its record for a job by this id, and its text.
    pub fn read_latest(&self, job: &JobId) -> Option<(u32, CheckedIn, String)> {
        let text = fs::read_to_string(self.latest()).ok()?;
        let (first, said) = text.split_once('\n')?;
        let mut fields = first.split('\t').map(str::parse::<u64>);
        let (Some(Ok(n)), Some(Ok(pid)), Some(Ok(every))) =
            (fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        let checked = CheckedIn {
            job: job.clone(),
            pid: u32::try_from(pid).ok()?,
            every,
        };
        Some((u32::try_from(n).ok()?, checked, said.to_string()))
    }
}

/// What a check-in names.
pub struct Job {
    pub id: JobId,
    pub dir: PathBuf,
    /// The runner's, which `--kill` takes.
    pub pid: u32,
}

pub struct CheckIn {
    end: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl CheckIn {
    /// Counts the job's running time a second at a time: time frozen is taken off as the freeze
    /// clock measured it, and a second is counted only when the job was not parked at either end.
    pub fn start(every: Duration, job: Job, stopper: Arc<Stopper>, frozen: Arc<Frozen>) -> Self {
        let (end, ended) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut ran = Duration::ZERO;
            let mut last = Instant::now();
            let mut was_frozen = frozen.total();
            let mut was_parked = false;
            let mut n = 0;
            while let Err(RecvTimeoutError::Timeout) = ended.recv_timeout(TICK) {
                let now = Instant::now();
                let frozen_now = frozen.total();
                let parked = matches!(stopper.state().stage, Stage::Parked);
                if !parked && !was_parked {
                    ran += (now - last).saturating_sub(frozen_now.saturating_sub(was_frozen));
                }
                (last, was_frozen, was_parked) = (now, frozen_now, parked);
                if ran >= every * (n + 1) {
                    n += 1;
                    job.check_in(n, ran, every, &stopper);
                }
            }
        });
        CheckIn { end, thread }
    }

    pub fn end(self) {
        let _ = self.end.send(());
        let _ = self.thread.join();
    }
}

impl Job {
    fn check_in(&self, n: u32, ran: Duration, every: Duration, stopper: &Stopper) {
        let check_ins = CheckIns { dir: &self.dir };
        let checked = CheckedIn {
            job: self.id.clone(),
            pid: self.pid,
            every: every.as_secs(),
        };
        let text = self.text(ran);
        check_ins.write_latest(n, &checked, &text);
        if n == 1 {
            Job::hand_over(&check_ins, checked, &text, stopper);
        }
    }

    fn hand_over(check_ins: &CheckIns, checked: CheckedIn, text: &str, stopper: &Stopper) {
        let Ok(handed) = File::create(check_ins.handed()) else {
            return;
        };
        if handed.try_lock().is_err() {
            return;
        }
        let handed_over = File::create(check_ins.report())
            .is_ok_and(|report| stopper.hand_over(checked, text, &report));
        match handed_over {
            true => {
                let _ = HANDED.set(handed);
            }
            false => {
                let _ = fs::remove_file(check_ins.handed());
            }
        }
    }

    fn text(&self, ran: Duration) -> String {
        let mut text = format!(
            "dibs: job {} has run {} (time frozen or parked not counted) and still runs. \
             This call lets go of it, and the job goes on.\n",
            self.id,
            Span(ran.as_secs())
        );
        text.push_str(&format!("  log, last {LOG_LINES} lines:\n"));
        text.push_str(&last_lines(&self.dir.join("log"), LOG_LINES));
        let mut services: Vec<PathBuf> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("with-") && n.ends_with(".log"))
            })
            .collect();
        services.sort();
        for path in services {
            let name = path
                .file_stem()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("with-"))
                .unwrap_or_default();
            text.push_str(&format!("  with {name}, last {SERVICE_LINES} lines:\n"));
            text.push_str(&last_lines(&path, SERVICE_LINES));
        }
        text
    }
}

/// The last lines of a file, read from no further back than its last `TAIL_BYTES`.
fn last_lines(path: &Path, lines: usize) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let from = file
        .metadata()
        .map_or(0, |m| m.len().saturating_sub(TAIL_BYTES));
    let mut end = Vec::new();
    if file.seek(SeekFrom::Start(from)).is_err() || file.read_to_end(&mut end).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&tail(&end, lines)).into_owned()
}
