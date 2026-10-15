//! A batch driven on its own machine, so it outlives the computer that sent it. What that
//! computer sent is under `~/.cache/dibs/detached/<id>`; the client driving it keeps its own
//! files under `~/.cache/dibs/detached/home`, apart from anyone's own dibs there.

use super::base::{Options, run};
use crate::{call::Driver, caller::Caller, execution::SentFrom, itself::Itself, paths::Paths};
use dibs_format::{BatchId, Exit, MachineName, Span};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::{BufRead as _, BufReader, Write as _},
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::Stdio,
};

/// What the sender says besides the files it sent, read by the batch's start.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Instructions {
    /// The driver's environment beyond what the start sets: the batch's machine, and who sent it.
    pub env: BTreeMap<String, String>,
    /// Where the driver starts, under what was sent: the copy of the repo the batch was sent
    /// from, or a directory named as it was, since a step naming no label is filed under it.
    pub cwd: PathBuf,
}

/// What a follow answers once its batch has ended.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Followed {
    /// The batch's directory on the machine, which the summary names.
    pub dir: String,
    /// None when the driver ended without writing one.
    pub summary: Option<String>,
    pub exit: Option<i32>,
    /// Each step's output and errors, by file name.
    pub files: BTreeMap<String, String>,
    /// The run records its steps wrote, one JSON line each.
    pub runs: String,
    /// The series file as its steps left it.
    pub series: String,
    /// What the driver said outside its steps, which is why when there is no summary.
    pub driver: String,
}

/// The detached batches on this machine, under `~/.cache/dibs/detached`.
pub struct Detached {
    dir: PathBuf,
}

impl Detached {
    /// The word a sender starts, drives and follows a batch with, outside the grammar.
    pub const WORD: &'static str = "__detached";
    /// Under the home directory.
    pub const DIR: &'static str = ".cache/dibs/detached";
    pub const BATCH: &'static str = "batch";
    pub const INSTRUCTIONS: &'static str = "detach.json";
    pub const ROOT: &'static str = "root";
    pub const RECIPES: &'static str = "recipes";
    pub const MACHINES: &'static str = "machines.toml";
    pub const SERIES: &'static str = "series";
    pub const SENT_FROM: &'static str = "sent-from";
    /// The copies a later batch's repos are sent against, so only what changed travels.
    pub const LAST: &'static str = "last";
    const HOME: &'static str = "home";
    const RUNS: &'static str = "runs.jsonl";
    const DRIVER: &'static str = "driver.err";
    const EXIT: &'static str = "exit";
    /// What the driver prints once it holds the batch, which is when its start returns.
    const DRIVING: &'static str = "driving";

    pub fn serve(words: &[String]) -> i32 {
        let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
            eprintln!("dibs: no HOME, so there is nowhere a detached batch is kept");
            return Exit::Refused.status();
        };
        let detached = Detached {
            dir: PathBuf::from(home).join(Detached::DIR),
        };
        match words {
            [verb, id] if verb == "start" => detached.start(&BatchId::new(id.as_str())),
            [verb, id] if verb == "drive" => detached.drive(&BatchId::new(id.as_str())),
            [verb, id] if verb == "follow" => detached.follow(&BatchId::new(id.as_str())),
            _ => {
                eprintln!(
                    "dibs: {} starts and follows a detached batch on its machine, and is not for calling directly",
                    Detached::WORD
                );
                Exit::Refused.status()
            }
        }
    }

    fn sent(&self, id: &BatchId) -> PathBuf {
        self.dir.join(id.as_str())
    }

    fn client(&self) -> Paths {
        let home = self.dir.join(Detached::HOME).into_os_string();
        Paths::from_vars(|name| (name == "DIBS_CLIENT_DIR").then(|| home.clone()))
    }

    /// Starts the driver in a session of its own, so it outlives the call that started it, and
    /// returns once it holds the batch.
    fn start(&self, id: &BatchId) -> i32 {
        let sent = self.sent(id);
        let instructions: Instructions = match fs::read(sent.join(Detached::INSTRUCTIONS))
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_slice(&text).map_err(|e| e.to_string()))
        {
            Ok(instructions) => instructions,
            Err(e) => {
                eprintln!("dibs: nothing usable was sent for batch {id}: {e}");
                return Exit::Refused.status();
            }
        };
        self.collect_old(id);
        let cwd = sent.join(&instructions.cwd);
        let errors = sent.join(Detached::DRIVER);
        let started = fs::create_dir_all(&cwd)
            .and_then(|()| File::create(&errors))
            .and_then(|errors| {
                let mut driver = Itself::command();
                driver
                    .args([Detached::WORD, "drive", id.as_str()])
                    .current_dir(&cwd)
                    .envs(self.environment(&sent))
                    .envs(&instructions.env)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(errors);
                // SAFETY: setsid is async-signal-safe.
                unsafe {
                    driver.pre_exec(|| {
                        libc::setsid();
                        Ok(())
                    });
                }
                driver.spawn()
            });
        let mut child = match started {
            Ok(child) => child,
            Err(e) => {
                eprintln!("dibs: the driver of batch {id} could not be started: {e}");
                return Exit::Failed.status();
            }
        };
        let mut said = String::new();
        if let Some(out) = child.stdout.take() {
            let _ = BufReader::new(out).read_line(&mut said);
        }
        if said.trim_end() == Detached::DRIVING {
            println!("its driver pid {} there", child.id());
            return 0;
        }
        let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
        eprint!("{}", fs::read_to_string(&errors).unwrap_or_default());
        eprintln!("dibs: the driver of batch {id} ended before it held the batch");
        match code {
            0 => Exit::Failed.status(),
            code => code,
        }
    }

    /// What the driver reads in place of the files of the computer that sent the batch.
    fn environment(&self, sent: &Path) -> Vec<(&'static str, OsString)> {
        let at = |name: &str| sent.join(name).into_os_string();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let cargo = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".cargo/bin").into_os_string())
            .unwrap_or_default();
        vec![
            (
                "DIBS_CLIENT_DIR",
                self.dir.join(Detached::HOME).into_os_string(),
            ),
            ("DIBS_ROOT", at(Detached::ROOT)),
            ("DIBS_RECIPES", at(Detached::RECIPES)),
            ("DIBS_MACHINES", at(Detached::MACHINES)),
            ("DIBS_SERIES", at(Detached::SERIES)),
            ("DIBS_RUNS", at(Detached::RUNS)),
            (SentFrom::VAR, at(Detached::SENT_FROM)),
            ("DIBS_LOCAL", "1".into()),
            ("PATH", [cargo, path].join(&OsString::from(":"))),
        ]
    }

    /// The batch, driven as `dibs batch` drives one, then its repos kept for the next batch to be
    /// sent against.
    fn drive(&self, id: &BatchId) -> i32 {
        let sent = self.sent(id);
        let text = match fs::read_to_string(sent.join(Detached::BATCH)) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("dibs: batch {id} has no steps here: {e}");
                return Exit::Refused.status();
            }
        };
        let opts = Options {
            dry_run: false,
            verbose: false,
            on: std::env::var("DIBS_ON")
                .ok()
                .filter(|on| !on.is_empty())
                .map(MachineName::new),
            owner: Some(Caller::from_env().id),
            id: Some(id.clone()),
            driving: Some(Detached::driving),
        };
        let code = run(&text, &opts).unwrap_or_else(|e| {
            eprintln!("dibs: {e}");
            Exit::Refused.status()
        });
        let _ = fs::write(sent.join(Detached::EXIT), format!("{code}\n"));
        self.keep_repos(&sent);
        code
    }

    /// Tells the start this process holds the batch, then lets go of the pipe it said so on: the
    /// start has returned, and a write there later would fail.
    fn driving() {
        println!("{}", Detached::DRIVING);
        let _ = std::io::stdout().flush();
        if let Ok(null) = File::options().write(true).open("/dev/null") {
            use std::os::fd::AsRawFd as _;
            // SAFETY: both descriptors are open; dup2 replaces stdout with /dev/null.
            unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) };
        }
    }

    /// Each repo the batch was sent becomes the copy the next batch's is sent against.
    fn keep_repos(&self, sent: &Path) {
        let last = self.dir.join(Detached::LAST);
        let Ok(repos) = fs::read_dir(sent.join(Detached::ROOT)) else {
            return;
        };
        let _ = fs::create_dir_all(&last);
        for repo in repos.flatten() {
            let kept = last.join(repo.file_name());
            let _ = fs::remove_dir_all(&kept);
            let _ = fs::rename(repo.path(), &kept);
        }
        let _ = fs::remove_dir_all(sent.join(Detached::ROOT));
    }

    /// What earlier batches were sent, once old enough that nobody follows them any more.
    fn collect_old(&self, id: &BatchId) {
        let keep = std::time::Duration::from_secs(14 * Span::DAY.0);
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if [Detached::HOME, Detached::LAST, id.as_str()]
                .iter()
                .any(|kept| name == *kept)
            {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > keep);
            if old {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Waits for the batch's driver to end, then prints what the sender keeps of it.
    fn follow(&self, id: &BatchId) -> i32 {
        let Some(dir) = self.client().batches().map(|d| d.join(id.as_str())) else {
            return Exit::Refused.status();
        };
        let sent = self.sent(id);
        if !sent.is_dir() {
            eprintln!("dibs: no batch {id} was detached here");
            return Exit::Refused.status();
        }
        // The driver holds this for as long as it runs, so taking it waits for the end.
        if let Ok(driver) = File::open(dir.join(Driver::RECORD)) {
            let _ = driver.lock_shared();
        }
        let read = |path: PathBuf| fs::read_to_string(path).unwrap_or_default();
        let files = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                (name.ends_with(".out") || name.ends_with(".err"))
                    .then(|| (name, read(entry.path())))
            })
            .collect();
        let followed = Followed {
            dir: dir.display().to_string(),
            summary: fs::read_to_string(dir.join("summary")).ok(),
            exit: read(sent.join(Detached::EXIT)).trim().parse().ok(),
            files,
            runs: read(sent.join(Detached::RUNS)),
            series: read(sent.join(Detached::SERIES)),
            driver: read(sent.join(Detached::DRIVER)),
        };
        match serde_json::to_string(&followed) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(e) => {
                eprintln!("dibs: batch {id} could not be told: {e}");
                Exit::Failed.status()
            }
        }
    }
}
