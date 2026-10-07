use crate::{
    call::OneLine as _,
    job::{Cap, Environment, Job, Output, ports::Ports, reap},
    sink::Sink,
};
use dibs_format::wire;
use std::{
    fs,
    net::{TcpStream, ToSocketAddrs as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

const READY_POLL: Duration = Duration::from_millis(200);
/// How long a readiness command is given to stop once its time is up.
const READY_GRACE: Duration = Duration::from_secs(2);
/// How much of a failed service's log is shown.
const LOG_TAIL: usize = 10;
/// How much of a service's command its record keeps.
const RECORD_COMMAND: usize = 160;
const LOCAL: &str = "127.0.0.1";

/// The `--with` servers of one call, run for the length of its job.
pub struct Services {
    list: Vec<Service>,
    /// `with.<pid>`, which status reads.
    record: PathBuf,
    started: Instant,
    ended: Option<Receiver<usize>>,
}

struct Service {
    name: String,
    ready: Option<String>,
    log: PathBuf,
    /// None when bash could not be started for it.
    pid: Option<u32>,
    status: Arc<OnceLock<i32>>,
    /// How it went, as the trailer says.
    end: String,
    bad: bool,
}

/// Where the services' logs go and what they are given.
pub struct Start<'a> {
    pub specs: &'a [wire::Service],
    pub environment: &'a Environment,
    /// The job's directory, or None when it has none, and their output goes nowhere.
    pub job_dir: Option<&'a Path>,
    pub record: PathBuf,
    pub sink: &'a Sink,
}

/// Watches the services while the job runs: the first to end stops the job.
pub struct Guard {
    state: Arc<Mutex<Watched>>,
}

#[derive(Default)]
struct Watched {
    over: bool,
    ended: Option<usize>,
}

impl Services {
    pub fn start(start: Start) -> Services {
        let (tell, ended) = mpsc::channel();
        let mut record = String::new();
        let list = start
            .specs
            .iter()
            .enumerate()
            .map(|(at, spec)| {
                let log = start.job_dir.map_or_else(
                    || PathBuf::from("/dev/null"),
                    |dir| dir.join(format!("with-{}.log", spec.name)),
                );
                let status = Arc::new(OnceLock::new());
                let pid = Job::spawn(
                    &spec.command,
                    start.environment,
                    Output::Log(&log),
                    start.sink,
                )
                .ok()
                .map(|job| {
                    let status = Arc::clone(&status);
                    let tell = tell.clone();
                    job.on_end(move |code| {
                        let _ = status.set(code);
                        let _ = tell.send(at);
                    })
                });
                if let Some(pid) = pid {
                    record.push_str(&format!(
                        "{}\t{pid}\t{}\n",
                        spec.name,
                        spec.command.one_line(RECORD_COMMAND)
                    ));
                }
                Service {
                    name: spec.name.clone(),
                    ready: spec.ready.clone(),
                    log,
                    pid,
                    status,
                    end: String::new(),
                    bad: false,
                }
            })
            .collect();
        let _ = fs::write(&start.record, record);
        Services {
            list,
            record: start.record,
            started: Instant::now(),
            ended: Some(ended),
        }
    }

    pub fn pids(&self) -> Vec<u32> {
        self.list.iter().filter_map(|s| s.pid).collect()
    }

    /// Each service in turn answers its readiness check, all within `within` seconds of their
    /// start; the first that exits first, or runs out of time, fails the call.
    pub fn ready(&mut self, within: u64, check: &Readiness) -> bool {
        let deadline = self.started + Duration::from_secs(within);
        for at in 0..self.list.len() {
            let mut answered = false;
            while !answered {
                let left = deadline
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_secs(1));
                answered = check.answers(self.list[at].ready.as_deref(), left);
                if answered {
                    continue;
                }
                let exited = match self.list[at].pid {
                    Some(_) => self.list[at].status.get().copied(),
                    None => Some(127),
                };
                if let Some(code) = exited {
                    self.failed(
                        at,
                        &format!("exited {code} before it was ready"),
                        "the command did not run",
                        check.sink,
                    );
                    return false;
                }
                if Instant::now() >= deadline {
                    self.failed(
                        at,
                        &format!("was not ready within {within}s"),
                        "the command did not run",
                        check.sink,
                    );
                    return false;
                }
                thread::sleep(READY_POLL);
            }
            self.list[at].end = format!("ready after {}s", self.started.elapsed().as_secs());
        }
        true
    }

    /// Stops the job when a service ends before it does.
    pub fn guard(&mut self, job: u32) -> Guard {
        let state = Arc::new(Mutex::new(Watched::default()));
        if let Some(ended) = self.ended.take() {
            let watched = Arc::clone(&state);
            thread::spawn(move || {
                for at in ended {
                    let mut state = watched.lock().unwrap_or_else(|e| e.into_inner());
                    if !state.over {
                        state.ended = Some(at);
                        reap(&[job]);
                        return;
                    }
                }
            });
        }
        Guard { state }
    }

    /// Says a service failed, with the end of its log, which is usually where the reason is.
    pub fn failed(&mut self, at: usize, what: &str, consequence: &str, sink: &Sink) {
        let service = &mut self.list[at];
        service.end = match service.end.is_empty() {
            true => what.to_string(),
            false => format!("{}, {what}", service.end),
        };
        service.bad = true;
        let mut said = format!("dibs: service {} {what}, so {consequence}.\n", service.name);
        let log = fs::read_to_string(&service.log).unwrap_or_default();
        if !log.is_empty() {
            said.push_str(&format!(
                "  The end of its log, {}:\n",
                service.log.display()
            ));
            let lines: Vec<&str> = log.lines().collect();
            for line in &lines[lines.len().saturating_sub(LOG_TAIL)..] {
                said.push_str(&format!("    {line}\n"));
            }
        }
        sink.say(&said);
    }

    /// What still runs is stopped, and the record goes.
    pub fn stop(&mut self) {
        let mut alive = Vec::new();
        for service in &mut self.list {
            let Some(pid) = service.pid.filter(|_| service.status.get().is_none()) else {
                continue;
            };
            alive.push(pid);
            service.end = match (service.bad, service.end.is_empty()) {
                (true, _) => format!("{}, and stopped", service.end),
                (false, true) => "stopped when the command ended".into(),
                (false, false) => format!("{}, stopped when the command ended", service.end),
            };
        }
        if !alive.is_empty() {
            reap(&alive);
        }
        let _ = fs::remove_file(&self.record);
    }

    /// The trailer's lines for them.
    pub fn lines(&self, host: &str) -> String {
        self.list
            .iter()
            .map(|s| {
                let end = match s.end.is_empty() {
                    true => "not started",
                    false => &s.end,
                };
                format!("  with {}: {end}  log {host}:{}\n", s.name, s.log.display())
            })
            .collect()
    }

    pub fn status(&self, at: usize) -> i32 {
        self.list[at].status.get().copied().unwrap_or(1)
    }

    /// The first service that has already ended.
    pub fn ended(&self) -> Option<usize> {
        self.list.iter().position(|s| s.status.get().is_some())
    }
}

impl Guard {
    /// The job has ended: the service that ended first, if one did.
    pub fn over(self) -> Option<usize> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.over = true;
        state.ended
    }
}

/// How a service is asked whether it can be used.
pub struct Readiness<'a> {
    pub ports: &'a Ports,
    pub environment: &'a Environment,
    pub sink: &'a Sink,
}

impl Readiness<'_> {
    /// None is ready at once; `tcp:[host:]port` connects; anything else is a command that exits 0.
    fn answers(&self, ready: Option<&str>, left: Duration) -> bool {
        let Some(ready) = ready else {
            return true;
        };
        let Some(address) = ready.strip_prefix("tcp:") else {
            let probe = Job::spawn(
                ready,
                self.environment,
                Output::Log(Path::new("/dev/null")),
                self.sink,
            );
            return probe.is_ok_and(|probe| {
                probe
                    .wait(Some(Cap {
                        after: left,
                        grace: READY_GRACE,
                    }))
                    .status
                    == 0
            });
        };
        let (host, port) = address.rsplit_once(':').unwrap_or((LOCAL, address));
        let Some(port) = self.ports.of(port) else {
            return false;
        };
        (host, port)
            .to_socket_addrs()
            .into_iter()
            .flatten()
            .any(|at| TcpStream::connect_timeout(&at, left).is_ok())
    }
}
