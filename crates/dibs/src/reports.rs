//! Friction filed where the people who fix dibs see it: an issue in a private repo the team
//! shares, with the answers brought back to the session that reported it.

use crate::{
    caller::Caller,
    cli::Friction,
    itself::Itself,
    paths::{FileError, Paths, ReportsStamp},
    records::{FrictionLog, RecordsError},
    update::{Build, ChangeNotice},
};
use dibs_format::{FrictionNote, Moment};
use dibs_runner::shared::SharedFile;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fmt,
    fs::{File, OpenOptions, TryLockError},
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    sync::mpsc,
    time::{Duration, Instant, SystemTime},
};

const LABEL: &str = "dibs-friction";
const FORWARD_READY: Duration = Duration::from_secs(30);
const STOPPED: &str = "DIBS-FORWARD-STOPPED";
const ENDED: &str = "DIBS-WAIT-ENDED";
/// Followed by the signal's number on the next line.
const SIGNALLED: &str = "DIBS-WAIT-SIGNALLED";
/// What gh says when the repo already has the webhook a forward makes.
const HOOK_TAKEN: &str = "Hook already exists";
/// A forwarder that lives less than this, this many times running, is not coming back.
const SHORT_LIVED: Duration = Duration::from_secs(60);
const SHORT_LIVES: u32 = 3;
const LISTED: &str = "200";
/// Replies are asked for at most this often unless DIBS_REPORTS_EVERY says otherwise, in minutes.
const ASKED_EVERY_MINUTES: u64 = 5;
/// A fetch that has not finished by then is given up, rather than left running.
const FETCH_LIMIT: Duration = Duration::from_secs(15);

/// Answers to this session's reports, fetched in the background on one call and printed on the
/// next, so no call waits on GitHub.
pub struct Notice<'a> {
    pub caller: &'a Caller,
}

impl Notice<'_> {
    pub fn tell(&self) {
        let paths = Paths::from_env();
        let Some(news) = paths.reports_news() else {
            return;
        };
        let mine = news.join(self.caller.file_name());
        if let Ok(text) = std::fs::read_to_string(&mine) {
            let _ = std::fs::remove_file(&mine);
            eprint!("{text}");
        }
        if ReportsRepo::from_env().is_none() {
            return;
        }
        let Some(stamp) = paths.reports(ReportsStamp::Asked) else {
            return;
        };
        let every = std::env::var("DIBS_REPORTS_EVERY")
            .ok()
            .and_then(|m| m.parse().ok())
            .unwrap_or(ASKED_EVERY_MINUTES);
        let asked_lately = std::fs::metadata(&stamp)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age < Duration::from_secs(every * 60));
        if asked_lately || std::fs::create_dir_all(&news).is_err() {
            return;
        }
        let _ = std::fs::write(&stamp, "");
        let mut fetch = Itself::command();
        fetch
            .args(["friction", "--replies"])
            .arg(&mine)
            .env("DIBS_FRICTION_BY", &self.caller.name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid only detaches the child from this terminal's signals.
        unsafe {
            fetch.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let _ = fetch.spawn();
    }
}

/// Why a report could not be filed, answered or waited for.
#[derive(Debug)]
pub enum ReportsError {
    /// DIBS_REPORTS is unset or names no repo.
    NoRepo,
    NoGh(io::Error),
    /// What gh said when it failed.
    Gh(String),
    /// A public repo, which a report must never reach.
    Public(String),
    /// gh created an issue and printed no URL for it.
    NoIssue(String),
    NoHome,
    File(FileError),
    Unreadable(serde_json::Error),
    NoForward(io::Error),
    NoStderr,
    /// The forwarder stopped before it listened, with what it said.
    Unlistening {
        repo: String,
        said: String,
    },
    /// Another listener on this computer holds the repo, under this pid.
    Listening {
        repo: String,
        pid: String,
    },
    NotReady,
    KeepsStopping(String),
    NoPort(io::Error),
    Io(io::Error),
    Records(RecordsError),
}

impl From<FileError> for ReportsError {
    fn from(e: FileError) -> ReportsError {
        ReportsError::File(e)
    }
}

impl From<RecordsError> for ReportsError {
    fn from(e: RecordsError) -> ReportsError {
        ReportsError::Records(e)
    }
}

impl fmt::Display for ReportsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReportsError::NoRepo => {
                f.write_str("DIBS_REPORTS names no <owner>/<repo> to take reports from")
            }
            ReportsError::NoGh(e) => write!(f, "could not run gh: {e}"),
            ReportsError::Gh(said) => f.write_str(said),
            ReportsError::Public(repo) => write!(
                f,
                "{repo} is public, and a report carries the paths and commands of whoever filed it"
            ),
            ReportsError::NoIssue(url) => write!(f, "gh printed no issue: {url}"),
            ReportsError::NoHome => f.write_str("no HOME to keep reports' state in"),
            ReportsError::File(e) => e.fmt(f),
            ReportsError::Unreadable(e) => {
                write!(f, "gh issue list printed something unreadable: {e}")
            }
            ReportsError::NoForward(e) => write!(f, "could not run gh webhook forward: {e}"),
            ReportsError::NoStderr => f.write_str("gh webhook forward has no stderr"),
            ReportsError::Unlistening { repo, said } if said.contains(HOOK_TAKEN) => write!(
                f,
                "GitHub already forwards {repo}'s webhooks to a listener, and none on this computer holds it: \
                 one on another computer, or one that died without removing its hook.\n  \
                 gh api repos/{repo}/hooks lists it; the one named cli is it."
            ),
            ReportsError::Unlistening { repo, said } => write!(
                f,
                "gh webhook forward stopped before it was listening. It needs the cli/gh-webhook extension, and \
                 admin on {repo}.\n  It said: {}",
                Forward::stop_reason(said)
            ),
            ReportsError::Listening { repo, pid } => write!(
                f,
                "a listener for {repo} already runs on this computer, pid {pid}, and GitHub forwards a repo's \
                 webhooks to one listener, so this one does not start.\n  \
                 If nothing reads it, as when the session that started it has ended, kill {pid} stops it, \
                 and it removes its webhook as it goes."
            ),
            ReportsError::NotReady => write!(
                f,
                "gh webhook forward did not start listening within {}s",
                FORWARD_READY.as_secs()
            ),
            ReportsError::KeepsStopping(said) => write!(
                f,
                "gh webhook forward keeps stopping as soon as it starts:\n{said}"
            ),
            ReportsError::NoPort(e) => write!(f, "no local port to listen on: {e}"),
            ReportsError::Io(e) => e.fmt(f),
            ReportsError::Records(e) => e.fmt(f),
        }
    }
}

/// The private repo `DIBS_REPORTS` names, `<owner>/<repo>`, where friction is filed and answered.
pub struct ReportsRepo {
    name: String,
}

impl ReportsRepo {
    /// Opt-in per person, since a report says what they were doing: without it, friction stays on
    /// the machine it was reported on.
    pub fn from_env() -> Option<ReportsRepo> {
        ReportsRepo::named(&std::env::var("DIBS_REPORTS").ok()?)
    }

    fn named(name: &str) -> Option<ReportsRepo> {
        let owned = matches!(name.split_once('/'), Some((owner, repo)) if !owner.is_empty() && !repo.is_empty() && !repo.contains('/'));
        owned.then(|| ReportsRepo {
            name: name.to_string(),
        })
    }

    /// Fetches the replies `by` has not seen and adds them to `into`, whole or not at all.
    pub fn fetch_replies(
        &self,
        notes: &[FrictionNote],
        by: &str,
        into: &Path,
    ) -> Result<(), ReportsError> {
        std::thread::spawn(|| {
            std::thread::sleep(FETCH_LIMIT);
            std::process::exit(124);
        });
        let fetched = self.replies(notes, by)?;
        SharedFile { path: into }
            .rewrite(|text| {
                let mut text = text.to_string();
                for reply in &fetched {
                    text.push_str(reply);
                    text.push('\n');
                }
                Some(text)
            })
            .map_err(ReportsError::Io)
    }

    /// A public repo is refused: a report carries the paths, repos and commands of whoever filed it.
    pub fn file(&self, n: &FrictionNote) -> Result<u64, ReportsError> {
        let repo = self.name.as_str();
        let visibility = gh(&[
            "repo",
            "view",
            repo,
            "--json",
            "visibility",
            "--jq",
            ".visibility",
        ])?;
        if visibility == "PUBLIC" {
            return Err(ReportsError::Public(repo.to_string()));
        }
        let title: String = n.text.chars().take(100).collect();
        let body = format!(
            "{}\n\n| | |\n|---|---|\n| reported by | {} |\n| dibs | {} |\n| from | {} |\n",
            n.text,
            n.by,
            n.version,
            host()
        );
        let create = [
            "issue", "create", "-R", repo, "--title", &title, "--body", &body, "--label", LABEL,
        ];
        let url = match gh(&create) {
            Err(ReportsError::Gh(said)) if said.contains(LABEL) => {
                gh(&[
                    "label",
                    "create",
                    LABEL,
                    "-R",
                    repo,
                    "--color",
                    "d4c5f9",
                    "--description",
                    "filed by dibs --friction",
                ])?;
                gh(&create)?
            }
            other => other?,
        };
        number(&url).ok_or(ReportsError::NoIssue(url))
    }

    fn listed(&self) -> Result<Vec<Value>, ReportsError> {
        let repo = self.name.as_str();
        let text = gh(&[
            "issue",
            "list",
            "-R",
            repo,
            "--label",
            LABEL,
            "--state",
            "all",
            "--limit",
            LISTED,
            "--json",
            "number,title,url,state,author,comments",
        ])?;
        let v: Value = serde_json::from_str(&text).map_err(ReportsError::Unreadable)?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Answers to the reports this session filed, each said once.
    fn replies(&self, notes: &[FrictionNote], by: &str) -> Result<Vec<String>, ReportsError> {
        let filed: BTreeSet<u64> = notes
            .iter()
            .filter(|n| n.by == by)
            .filter_map(|n| n.issue)
            .collect();
        if filed.is_empty() {
            return Ok(Vec::new());
        }
        let mut shown = Keys::load(ReportsStamp::Shown)?;
        let own = ReportsRepo::own_login();
        let mut out = Vec::new();
        for issue in self.listed()? {
            let Some(n) = issue["number"].as_u64().filter(|n| filed.contains(n)) else {
                continue;
            };
            for c in issue["comments"].as_array().into_iter().flatten() {
                let url = c["url"].as_str().unwrap_or_default();
                if url.is_empty() || posted_by(&c["author"], own.as_deref()) || !shown.insert(url) {
                    continue;
                }
                out.push(format!(
                    "dibs: your report #{n} was answered by {}: {}\n  {url}",
                    login(&c["author"]),
                    first_line(&c["body"])
                ));
            }
        }
        shown.save()?;
        Ok(out)
    }

    /// Everything not yet woken on. The first time, that is the open reports, and the rest is taken
    /// as read rather than replayed.
    fn catch_up(&self, woken: &mut Keys, own: Option<&str>) -> Result<Vec<String>, ReportsError> {
        let first = woken.fresh;
        let mut out = Vec::new();
        for issue in self.listed()? {
            let n = issue["number"].as_u64().unwrap_or_default();
            let open = issue["state"].as_str() == Some("OPEN");
            if woken.insert(&format!("#{n}")) && (!first || open) {
                out.push(format!(
                    "report #{n} from {}: {}\n  {}",
                    login(&issue["author"]),
                    issue["title"].as_str().unwrap_or_default(),
                    issue["url"].as_str().unwrap_or_default()
                ));
            }
            for c in issue["comments"].as_array().into_iter().flatten() {
                let url = c["url"].as_str().unwrap_or_default();
                if url.is_empty() || posted_by(&c["author"], own) || !woken.insert(url) || first {
                    continue;
                }
                out.push(format!(
                    "comment on #{n} from {}: {}\n  {url}",
                    login(&c["author"]),
                    first_line(&c["body"])
                ));
            }
        }
        woken.fresh = false;
        Ok(out)
    }

    /// Returns once a report, or a comment from another account, lands, or with nothing once
    /// `max` has passed. What landed while nothing listened is read first, since GitHub
    /// delivers a webhook once or not at all.
    pub fn wait(&self, max: Option<Duration>) -> Result<Vec<String>, ReportsError> {
        let endings = Endings::held();
        let listening = Listening::take(self)?;
        let mut woken = Keys::load(ReportsStamp::Woken)?;
        let own = ReportsRepo::own_login();
        let own = own.as_deref();
        let news = self.catch_up(&mut woken, own)?;
        if !news.is_empty() {
            woken.save()?;
            return Ok(news);
        }
        woken.save()?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(ReportsError::NoPort)?;
        let port = listener.local_addr().map_err(ReportsError::Io)?.port();
        endings.told_to(port);
        if let Some(max) = max {
            std::thread::spawn(move || {
                std::thread::sleep(max);
                if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                    let _ = writeln!(s, "{ENDED}");
                }
            });
        }
        let mut short = 0;
        let mut said = String::new();
        while short < SHORT_LIVES {
            let started = Instant::now();
            let forward = Forward::start(self, port)?;
            let news = self.catch_up(&mut woken, own)?;
            if !news.is_empty() {
                woken.save()?;
                return Ok(news);
            }
            said = match Forwarded::listen(&listener, &mut woken, own) {
                Forwarded::News(news) => {
                    woken.save()?;
                    return Ok(news);
                }
                Forwarded::Ended => {
                    eprintln!(
                        "dibs: nothing landed within {}s, so this listener ends; start it again to go on listening",
                        max.unwrap_or_default().as_secs()
                    );
                    return Ok(Vec::new());
                }
                Forwarded::Signalled(ending) => {
                    drop(forward);
                    drop(listening);
                    eprintln!(
                        "dibs: {} stopped this listener, and its webhook on {self} is removed",
                        ending.name()
                    );
                    ending.raise();
                }
                Forwarded::Stopped(said) => said,
            };
            drop(forward);
            short = match started.elapsed() < SHORT_LIVED {
                true => short + 1,
                false => 0,
            };
            if short < SHORT_LIVES {
                eprintln!(
                    "dibs: gh webhook forward stopped, so it is started again. It said: {}",
                    Forward::stop_reason(&said)
                );
            }
        }
        Err(ReportsError::KeepsStopping(said.trim().to_string()))
    }

    fn reply(&self, issue: u64, text: &str, close: bool) -> Result<String, ReportsError> {
        let repo = self.name.as_str();
        let n = issue.to_string();
        let url = gh(&["issue", "comment", &n, "-R", repo, "--body", text])?;
        if close {
            gh(&["issue", "close", &n, "-R", repo])?;
        }
        Ok(url)
    }

    /// The account gh acts as here, which every reply from here is posted under. None when gh
    /// cannot say, and then every comment counts as news.
    fn own_login() -> Option<String> {
        gh(&["api", "user", "--jq", ".login"])
            .ok()
            .filter(|login| !login.is_empty())
    }
}

impl fmt::Display for ReportsRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

fn gh(args: &[&str]) -> Result<String, ReportsError> {
    let out = Command::new("gh")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(ReportsError::NoGh)?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        false => Err(ReportsError::Gh(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )),
    }
}

fn host() -> String {
    Command::new("hostname")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn number(url: &str) -> Option<u64> {
    url.trim().rsplit('/').next()?.parse().ok()
}

/// What has already been said or acted on, one key a line: `#<n>` for a report, a comment's URL
/// for a comment.
struct Keys {
    path: PathBuf,
    set: BTreeSet<String>,
    fresh: bool,
}

impl Keys {
    fn load(stamp: ReportsStamp) -> Result<Keys, ReportsError> {
        let path = Paths::from_env()
            .reports(stamp)
            .ok_or(ReportsError::NoHome)?;
        let text = std::fs::read_to_string(&path);
        let fresh = text.is_err();
        let set = text
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        Ok(Keys { path, set, fresh })
    }

    fn insert(&mut self, key: &str) -> bool {
        self.set.insert(key.to_string())
    }

    fn save(&self) -> Result<(), ReportsError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(FileError::at(dir))?;
        }
        let text: String = self.set.iter().map(|k| format!("{k}\n")).collect();
        Ok(std::fs::write(&self.path, text).map_err(FileError::at(&self.path))?)
    }
}

fn first_line(body: &Value) -> String {
    let line = body
        .as_str()
        .unwrap_or_default()
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    line.chars().take(200).collect()
}

fn login(v: &Value) -> &str {
    v["login"].as_str().unwrap_or("someone")
}

/// Whether a comment's author is this computer's own GitHub account. Its replies are known here
/// already, and every other account's are news, a reporter answering through dibs included.
fn posted_by(author: &Value, own: Option<&str>) -> bool {
    own.is_some_and(|own| author["login"].as_str() == Some(own))
}

fn labelled(issue: &Value) -> bool {
    issue["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|l| l["name"].as_str() == Some(LABEL))
}

fn on_event(event: &str, body: &Value, woken: &mut Keys, own: Option<&str>) -> Vec<String> {
    let issue = &body["issue"];
    if !labelled(issue) {
        return Vec::new();
    }
    let n = issue["number"].as_u64().unwrap_or_default();
    match (event, body["action"].as_str().unwrap_or_default()) {
        ("issues", "opened" | "reopened") if woken.insert(&format!("#{n}")) => vec![format!(
            "report #{n} from {}: {}\n  {}",
            login(&issue["user"]),
            issue["title"].as_str().unwrap_or_default(),
            issue["html_url"].as_str().unwrap_or_default()
        )],
        ("issue_comment", "created") => {
            let c = &body["comment"];
            let url = c["html_url"].as_str().unwrap_or_default();
            if url.is_empty() || posted_by(&c["user"], own) || !woken.insert(url) {
                return Vec::new();
            }
            vec![format!(
                "comment on #{n} from {}: {}\n  {url}",
                login(&c["user"]),
                first_line(&c["body"])
            )]
        }
        _ => Vec::new(),
    }
}

/// What reached the listener: a webhook, or word from the forwarder's reader that it stopped,
/// since a forwarder that dies otherwise leaves the wait listening to nothing, for good.
enum Arrival {
    Delivery(String, Value),
    Stopped(String),
    /// The wait's `max` passed.
    Ended,
    Signalled(Ending),
}

impl Arrival {
    /// A webhook is answered, so the forwarder moves on.
    fn new(stream: TcpStream) -> Option<Arrival> {
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut first = String::new();
        reader.read_line(&mut first).ok()?;
        match first.trim_end() {
            STOPPED => {
                let mut said = String::new();
                let _ = reader.read_to_string(&mut said);
                return Some(Arrival::Stopped(said));
            }
            ENDED => return Some(Arrival::Ended),
            SIGNALLED => {
                let mut signal = String::new();
                reader.read_line(&mut signal).ok()?;
                return Some(Arrival::Signalled(Ending(signal.trim().parse().ok()?)));
            }
            _ => {}
        }
        let (mut event, mut length) = (String::new(), 0usize);
        for header in Arrival::headers(&mut reader)? {
            if let Some((k, v)) = header.split_once(':') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "x-github-event" => event = v.trim().to_string(),
                    "content-length" => length = v.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
        let _ = (&stream)
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        Some(Arrival::Delivery(
            event,
            serde_json::from_slice(&body).ok()?,
        ))
    }

    /// The headers up to the blank line that ends them; None when the stream ends first.
    fn headers(reader: &mut impl BufRead) -> Option<Vec<String>> {
        let mut headers = Vec::new();
        for line in reader.lines() {
            match line.ok()?.trim_end() {
                "" => return Some(headers),
                header => headers.push(header.to_string()),
            }
        }
        None
    }
}

/// This computer's one listener for a repo, held while it listens.
struct Listening {
    /// Locked for as long as it is open.
    _held: File,
}

impl Listening {
    fn take(repo: &ReportsRepo) -> Result<Listening, ReportsError> {
        let path = Paths::from_env()
            .reports_listener(&repo.name)
            .ok_or(ReportsError::NoHome)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(FileError::at(dir))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(FileError::at(&path))?;
        match file.try_lock() {
            Ok(()) => {
                file.set_len(0).map_err(FileError::at(&path))?;
                write!(file, "{}", std::process::id()).map_err(FileError::at(&path))?;
                Ok(Listening { _held: file })
            }
            Err(TryLockError::WouldBlock) => Err(ReportsError::Listening {
                repo: repo.to_string(),
                pid: std::fs::read_to_string(&path)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            }),
            Err(TryLockError::Error(e)) => Err(FileError::new(&path, e).into()),
        }
    }
}

/// What one forwarder brought before it stopped: news, what it said as it stopped, or the end of
/// the wait.
enum Forwarded {
    News(Vec<String>),
    Stopped(String),
    Ended,
    Signalled(Ending),
}

impl Forwarded {
    /// Listens until a delivery is news, the forwarder says it stopped, or the wait ends.
    fn listen(listener: &TcpListener, woken: &mut Keys, own: Option<&str>) -> Forwarded {
        listener
            .incoming()
            .filter_map(Result::ok)
            .filter_map(Arrival::new)
            .find_map(|arrival| match arrival {
                Arrival::Stopped(said) => Some(Forwarded::Stopped(said)),
                Arrival::Ended => Some(Forwarded::Ended),
                Arrival::Signalled(ending) => Some(Forwarded::Signalled(ending)),
                Arrival::Delivery(event, body) => {
                    let news = on_event(&event, &body, woken, own);
                    (!news.is_empty()).then_some(Forwarded::News(news))
                }
            })
            // `incoming` never ends; were it to, the forwarder would be started again.
            .unwrap_or_else(|| Forwarded::Stopped(String::new()))
    }
}

/// What ends a listener's session, held back from every thread so that one takes it and ends the
/// wait as its `max` does: a process killed by it would leave the forward's webhook on the repo,
/// refusing the next listener.
struct Endings(libc::sigset_t);

/// One of the endings, taken.
#[derive(Debug, Clone, Copy)]
struct Ending(libc::c_int);

impl Endings {
    const SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

    /// Held back from this thread and every thread it starts from now on.
    fn held() -> Endings {
        // SAFETY: the set is initialised by sigemptyset before anything reads it.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for signal in Endings::SIGNALS {
                libc::sigaddset(&mut set, signal);
            }
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            Endings(set)
        }
    }

    /// The first to arrive is told to the wait listening on `port`.
    fn told_to(self, port: u16) {
        std::thread::spawn(move || {
            let mut signal = 0;
            // SAFETY: the set is initialised, and sigwait writes the signal it took.
            let took = unsafe { libc::sigwait(&self.0, &mut signal) } == 0;
            if took && let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = writeln!(s, "{SIGNALLED}\n{signal}");
            }
        });
    }
}

impl Ending {
    fn name(self) -> &'static str {
        match self.0 {
            libc::SIGTERM => "SIGTERM",
            libc::SIGHUP => "SIGHUP",
            _ => "SIGINT",
        }
    }

    /// Dies of it, as it would have killed the process, so whoever sent it reads that status.
    fn raise(self) -> ! {
        // SAFETY: restores the default action for the signal, unblocks it and raises it here.
        unsafe {
            libc::signal(self.0, libc::SIG_DFL);
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, self.0);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            libc::raise(self.0);
        }
        std::process::exit(128 + self.0)
    }
}

/// `gh webhook forward`, which relays the repo's webhooks over its own connection to GitHub, so
/// nothing here has to be reachable from outside. gh never deletes the webhook it made, and GitHub
/// keeps one whose connection dropped, refusing the next forward, so this deletes it when dropped.
struct Forward {
    gh: Child,
    /// The webhook's API path, once gh has made it.
    hook: Option<String>,
}

impl Forward {
    fn start(repo: &ReportsRepo, port: u16) -> Result<Forward, ReportsError> {
        let mut child = Command::new("gh")
            .args([
                "webhook",
                "forward",
                "--repo",
                &repo.name,
                "--events",
                "issues,issue_comment",
                "--url",
                &format!("http://127.0.0.1:{port}/"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ReportsError::NoForward)?;
        let stderr = child.stderr.take().ok_or(ReportsError::NoStderr)?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut said = String::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.contains("Forwarding") {
                    let _ = tx.send(Ok(()));
                }
                said.push_str(&line);
                said.push('\n');
            }
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = write!(s, "{STOPPED}\n{said}");
            }
            let _ = tx.send(Err(said));
        });
        let mut forward = Forward {
            gh: child,
            hook: None,
        };
        match rx.recv_timeout(FORWARD_READY) {
            Ok(Ok(())) => {
                forward.hook = Forward::hook(repo);
                Ok(forward)
            }
            Ok(Err(said)) => Err(ReportsError::Unlistening {
                repo: repo.to_string(),
                said: said.trim().to_string(),
            }),
            Err(_) => Err(ReportsError::NotReady),
        }
    }

    /// One forward per repo can run, so the forwarder's webhook is the repo's only one gh made.
    fn hook(repo: &ReportsRepo) -> Option<String> {
        let hooks = format!("repos/{}/hooks", repo.name);
        let id = gh(&[
            "api",
            &hooks,
            "--jq",
            r#".[] | select(.name == "cli") | .id"#,
        ])
        .ok()?;
        let id = id.lines().next().filter(|id| !id.is_empty())?;
        Some(format!("{hooks}/{id}"))
    }

    /// gh prints its usage after an error, so the last line it wrote is a flag's description.
    fn stop_reason(said: &str) -> &str {
        let mut lines = said.lines().map(str::trim).filter(|l| !l.is_empty());
        let first = lines.clone().next().unwrap_or("nothing");
        lines
            .find(|l| l.to_ascii_lowercase().starts_with("error"))
            .unwrap_or(first)
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.gh.kill();
        let _ = self.gh.wait();
        if let Some(hook) = &self.hook {
            let _ = gh(&["api", "-X", "DELETE", hook]);
        }
    }
}

/// The text arrives in the environment rather than as an argument: a report about a flag starts
/// with the flag, and parsing that as one is how the complaint becomes the complaint.
pub fn friction_verb(friction: Friction) -> Result<ExitCode, ReportsError> {
    let reports_repo = || ReportsRepo::from_env().ok_or(ReportsError::NoRepo);
    match friction {
        Friction::Wait { max } => {
            for news in reports_repo()?.wait(max.map(Duration::from_secs))? {
                println!("{news}");
            }
        }
        Friction::Reply {
            issue,
            answer,
            close,
        } => println!("{}", reports_repo()?.reply(issue, &answer, close)?),
        Friction::Note { text } => {
            let caller = Caller::from_env();
            ChangeNotice::tell_once(&caller);
            let mut note = FrictionNote::new(
                &text,
                &caller.name,
                Build::COMMIT.unwrap_or_default(),
                Moment::epoch_now(),
            )
            .ok_or(RecordsError::EmptyNote)?;
            let filed = ReportsRepo::from_env().map(|repo| (repo.file(&note), repo));
            if let Some((Ok(n), _)) = &filed {
                note.issue = Some(*n);
            }
            FrictionLog::here()?.append(&note)?;
            match filed {
                None => println!(
                    "Recorded. dibs gaps prints it, with everything else that got in the way."
                ),
                Some((Ok(n), repo)) => println!(
                    "Recorded, and filed as {repo}#{n}. An answer there shows on a later dibs call."
                ),
                Some((Err(e), repo)) => println!("Recorded here, but not filed in {repo}: {e}"),
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stopped_forwarder_is_reported_by_its_error_not_its_usage() {
        let said = "Error: websocket closed\nUsage:\n  forward --events=<types>\n  -U, --url string   Address of the local server\n";
        assert_eq!(Forward::stop_reason(said), "Error: websocket closed");
        assert_eq!(
            Forward::stop_reason("\n  connection reset\n"),
            "connection reset"
        );
        assert_eq!(Forward::stop_reason(""), "nothing");
    }

    fn keys() -> Keys {
        Keys {
            path: PathBuf::new(),
            set: BTreeSet::new(),
            fresh: false,
        }
    }

    #[test]
    fn a_comment_wakes_once_and_never_on_one_this_account_posted() {
        let body = |u: &str, by: &str, text: &str| {
            serde_json::json!({
                "action": "created",
                "issue": { "number": 3, "labels": [{ "name": LABEL }] },
                "comment": { "html_url": u, "user": { "login": by }, "body": text },
            })
        };
        let own = Some("maintainer");
        let url = "https://github.com/o/r/issues/3#issuecomment-9";
        let mut woken = keys();
        assert_eq!(
            on_event(
                "issue_comment",
                &body(url, "someone", "still broken\nmore"),
                &mut woken,
                own
            ),
            vec![format!("comment on #3 from someone: still broken\n  {url}")]
        );
        assert!(
            on_event(
                "issue_comment",
                &body(url, "someone", "still broken"),
                &mut woken,
                own
            )
            .is_empty(),
            "once"
        );
        assert!(
            on_event(
                "issue_comment",
                &body(
                    "https://github.com/o/r/issues/3#issuecomment-10",
                    "maintainer",
                    "fixed in abc"
                ),
                &mut woken,
                own
            )
            .is_empty(),
            "not its own"
        );
        let through_dibs = "the source was an absolute path\n\n<!-- dibs --friction --reply -->";
        assert_eq!(
            on_event(
                "issue_comment",
                &body(
                    "https://github.com/o/r/issues/3#issuecomment-11",
                    "someone",
                    through_dibs
                ),
                &mut woken,
                own
            )
            .len(),
            1,
            "a reporter answering through dibs is news"
        );
    }

    #[test]
    fn only_a_labelled_issue_opening_is_a_report() {
        let opened = |labels: Value, action: &str| {
            serde_json::json!({
                "action": action,
                "issue": { "number": 5, "labels": labels, "title": "t", "html_url": "u", "user": { "login": "someone" } },
            })
        };
        let mut woken = keys();
        assert!(
            on_event(
                "issues",
                &opened(serde_json::json!([]), "opened"),
                &mut woken,
                None
            )
            .is_empty(),
            "unlabelled"
        );
        assert!(
            on_event(
                "issues",
                &opened(serde_json::json!([{ "name": LABEL }]), "closed"),
                &mut woken,
                None
            )
            .is_empty(),
            "closing"
        );
        assert_eq!(
            on_event(
                "issues",
                &opened(serde_json::json!([{ "name": LABEL }]), "opened"),
                &mut woken,
                None
            )
            .len(),
            1
        );
    }

    #[test]
    fn a_repo_is_owner_and_name() {
        for (v, ok) in [
            ("o/r", true),
            ("r", false),
            ("o/r/x", false),
            ("/r", false),
            ("o/", false),
        ] {
            assert_eq!(ReportsRepo::named(v).is_some(), ok, "{v}");
        }
    }
}
