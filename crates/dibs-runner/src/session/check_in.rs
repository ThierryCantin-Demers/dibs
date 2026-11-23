//! A job's call lets go of it once it has run as long as `--check-in` gives it, so the agent that
//! started it hears of it, while the job goes on; `dibs --follow` hears each check-in after.

use crate::{ahead::Frozen, kept::tail, stop::Stage, stop::Stopper};
use dibs_format::{JobId, Span};
use std::{
    fs::{self, File},
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

    /// The latest check-in, its number on the first line.
    pub fn latest(&self) -> PathBuf {
        self.dir.join("check-in")
    }

    /// The number and the text of the latest check-in.
    pub fn read_latest(&self) -> Option<(u64, String)> {
        let text = fs::read_to_string(self.latest()).ok()?;
        let (n, said) = text.split_once('\n')?;
        Some((n.parse().ok()?, said.to_string()))
    }
}

/// What a check-in names.
pub struct Job {
    pub id: JobId,
    pub dir: PathBuf,
    pub host: String,
    /// The runner's, which `--kill` takes.
    pub pid: u32,
}

pub struct CheckIn {
    end: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl CheckIn {
    pub fn start(every: Duration, job: Job, stopper: Arc<Stopper>, frozen: Arc<Frozen>) -> Self {
        let (end, ended) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut ran = Duration::ZERO;
            let mut last = Instant::now();
            let mut n = 0;
            while let Err(RecvTimeoutError::Timeout) = ended.recv_timeout(TICK) {
                let now = Instant::now();
                let counts = !frozen.now() && !matches!(stopper.state().stage, Stage::Parked);
                if counts {
                    ran += now - last;
                }
                last = now;
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
        let kept = CheckIns { dir: &self.dir };
        let text = self.text(ran, every);
        let part = kept.latest().with_extension("part");
        if fs::write(&part, format!("{n}\n{text}")).is_ok() {
            let _ = fs::rename(&part, kept.latest());
        }
        if n == 1 {
            self.hand_over(&kept, &text, stopper);
        }
    }

    fn hand_over(&self, kept: &CheckIns, text: &str, stopper: &Stopper) {
        let Ok(handed) = File::create(kept.handed()) else {
            return;
        };
        if handed.try_lock().is_err() {
            return;
        }
        let handed_over =
            File::create(kept.report()).is_ok_and(|report| stopper.hand_over(text, &report));
        match handed_over {
            true => {
                let _ = HANDED.set(handed);
            }
            false => {
                let _ = fs::remove_file(kept.handed());
            }
        }
    }

    fn text(&self, ran: Duration, every: Duration) -> String {
        let (host, job) = (&self.host, &self.id);
        let mut text = format!(
            "dibs: job {job} has run {} (time frozen or parked not counted) and still runs. \
             This call lets go of it, and the job goes on.\n",
            Span(ran.as_secs())
        );
        let log = fs::read(self.dir.join("log")).unwrap_or_default();
        text.push_str(&format!("  log, last {LOG_LINES} lines:\n"));
        text.push_str(&String::from_utf8_lossy(&tail(&log, LOG_LINES)));
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
            let log = fs::read(&path).unwrap_or_default();
            text.push_str(&format!("  with {name}, last {SERVICE_LINES} lines:\n"));
            text.push_str(&String::from_utf8_lossy(&tail(&log, SERVICE_LINES)));
        }
        text.push_str(&format!(
            "  dibs --on {host} --follow {job}  waits for it: it ends with the job, or after another {} of running\n  \
             dibs --on {host} --kill {}  stops it\n",
            Span(every.as_secs()),
            self.pid
        ));
        text
    }
}
