use crate::machine::{
    deadline::Deadline,
    held::Holder,
    interrupt::Interrupt,
    served::{Delivery, Served},
    ssh::Ssh,
    target::Target,
    unreachable::Unreachable,
    values::CallValues,
};
use dibs_format::Exit;
use std::{
    io,
    net::Ipv4Addr,
    path::Path,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

/// A caller that says nothing for this long is gone, unless `DIBS_LEASE` says otherwise.
const DEFAULT_LEASE_SECS: u64 = 120;
/// ssh's own failure, never the command's.
pub const SSH_FAILED: i32 = 255;
/// This computer, and whether every call stays on it.
#[derive(Debug, Clone)]
pub struct Here {
    /// Its name up to the first dot.
    pub name: String,
    /// `DIBS_LOCAL=1`.
    pub local: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The lock is taken on this computer.
    Here,
    Ssh {
        host: String,
    },
}

/// One call to one machine.
#[derive(Debug, Clone)]
pub struct Session {
    pub route: Route,
    /// The machine whose lock the call takes, lowercased.
    pub lock_at: String,
    /// What notices call the machine: its inventory name, or the host.
    pub name: String,
    pub said: Said,
}

/// What a call's last attempt heard on stderr outside the runner's frames: ssh's reason, when
/// ssh is what failed, kept so that reason is not asked of the machine a second time.
#[derive(Debug, Clone, Default)]
pub struct Said(Arc<Mutex<Vec<u8>>>);

impl Said {
    /// Only the end is kept: a reason is ssh's last words.
    const KEPT: usize = 4096;

    pub fn clear(&self) {
        self.kept().clear();
    }

    pub fn add(&self, bytes: &[u8]) {
        let mut kept = self.kept();
        kept.extend_from_slice(bytes);
        let over = kept.len().saturating_sub(Said::KEPT);
        kept.drain(..over);
    }

    fn kept(&self) -> MutexGuard<'_, Vec<u8>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

/// Where a command run here reaches the machine a session locks.
#[derive(Debug, Clone)]
pub enum Reach {
    Known(String),
    /// The address ssh dials for the host, asked only when a service needs it.
    Dialled {
        host: String,
        otherwise: String,
    },
}

impl Reach {
    pub fn address(&self) -> String {
        match self {
            Reach::Known(name) => name.clone(),
            Reach::Dialled { host, otherwise } => {
                Ssh::dials(host).unwrap_or_else(|| otherwise.clone())
            }
        }
    }
}

/// The caller's half of a job dying with it, from the environment.
#[derive(Debug, Clone, Copy)]
pub struct Liveness {
    /// `DIBS_NO_LIVE=1`: the runner does not watch the caller over the channel.
    pub no_live: bool,
    /// `DIBS_NO_WATCHDOG=1`: the machine does not watch the channel.
    pub no_watchdog: bool,
    pub lease: u64,
}

/// What the channel carries besides heartbeats.
#[derive(Debug, Clone, Copy)]
pub enum Message {
    /// A held command ended with this status, which is not the caller going away.
    Release(i32),
}

/// Which of a machine half's streams an answer keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// Stdout, with stderr passed through to this process's.
    Stdout,
    /// Stdout, with stderr dropped.
    StdoutAlone,
    /// Both, interleaved as they were written, with what this side says about the machine.
    Everything,
}

/// What a machine said within the bound it was given.
#[derive(Debug)]
pub struct Answer {
    pub output: Vec<u8>,
    /// None when the bound passed first and the call was stopped.
    pub exit: Option<i32>,
}

/// The exit a call gives for its machine half's, and why when that is not the command's own.
#[derive(Debug)]
pub struct Diagnosis {
    pub exit: i32,
    pub said: String,
}

impl Liveness {
    pub fn from_env() -> Liveness {
        let on = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        let lease = std::env::var("DIBS_LEASE")
            .ok()
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_LEASE_SECS);
        Liveness {
            no_live: on("DIBS_NO_LIVE"),
            no_watchdog: on("DIBS_NO_WATCHDOG"),
            lease,
        }
    }
}

impl Session {
    /// Inside a `--hold` of this machine's lock, a call that takes it again queues behind the
    /// hold, which only ends when the call does.
    pub fn inside_hold(&self) -> bool {
        let held = std::env::var("DIBS_HOLDING").unwrap_or_default();
        format!(" {} ", held.to_ascii_lowercase()).contains(&format!(" {} ", self.lock_at))
    }

    pub fn new(target: &Target, here: &Here) -> Session {
        let me = here.name.to_ascii_lowercase();
        let mut session = match me == target.hostname.to_ascii_lowercase() || here.local {
            true => Session {
                route: Route::Here,
                lock_at: me,
                name: String::new(),
                said: Said::default(),
            },
            false => Session {
                route: Route::Ssh {
                    host: target.host.clone(),
                },
                lock_at: target.hostname.to_ascii_lowercase(),
                name: String::new(),
                said: Said::default(),
            },
        };
        session.name = session.at(target, here);
        session
    }

    /// On this computer, loopback: its own short name need not resolve, and a Mac's does not.
    pub fn reach(&self, target: &Target, here: &Here) -> Reach {
        match &self.route {
            Route::Here => Reach::Known(Ipv4Addr::LOCALHOST.to_string()),
            Route::Ssh { host } => Reach::Dialled {
                host: host.clone(),
                otherwise: match target.hostname.is_empty() {
                    true => here.name.clone(),
                    false => target.hostname.clone(),
                },
            },
        }
    }

    /// Where notices say the lock is: the machine's name, or this computer's.
    pub fn at(&self, target: &Target, here: &Here) -> String {
        match (&self.route, &target.machine) {
            (Route::Here, _) => here.name.clone(),
            (Route::Ssh { .. }, Some(machine)) => machine.to_string(),
            (Route::Ssh { .. }, None) if !target.hostname.is_empty() => target.hostname.clone(),
            (Route::Ssh { .. }, None) => here.name.clone(),
        }
    }

    /// The exit a call gives for its machine half's: ssh's own failures are diagnosed here.
    pub fn exit(&self, status: i32, target: &Target) -> i32 {
        let diagnosis = self.diagnose(status, target);
        eprint!("{}", diagnosis.said);
        diagnosis.exit
    }

    /// The exit for a machine half's, and what to say about it.
    pub fn diagnose(&self, status: i32, target: &Target) -> Diagnosis {
        match (&self.route, status) {
            (Route::Ssh { .. }, SSH_FAILED) if !Interrupt::heard() => Diagnosis {
                exit: i32::from(Exit::Unreachable.code()),
                said: Unreachable {
                    target,
                    said: self.said.text(),
                }
                .diagnosis(),
            },
            (_, exit) => Diagnosis {
                exit,
                said: String::new(),
            },
        }
    }

    /// Runs a call whose command runs on the machine, and returns its exit.
    pub fn run(&self, values: &CallValues, live: Liveness) -> io::Result<i32> {
        self.served(values, live).run(Delivery::Inherit)
    }

    fn served<'a>(&'a self, values: &'a CallValues, live: Liveness) -> Served<'a> {
        Served {
            session: self,
            values,
            live,
            holding: None,
            deadline: None,
        }
    }

    /// Runs a call whose output this process reads, a line at a time, as it arrives.
    pub fn run_reading(
        &self,
        values: &CallValues,
        live: Liveness,
        delivery: Delivery,
    ) -> io::Result<i32> {
        self.served(values, live).run(delivery)
    }

    /// Runs a call whose output this process reads a line at a time, until it ends or its
    /// deadline stops it.
    pub fn read_until(
        &self,
        values: &CallValues,
        delivery: Delivery,
        deadline: Deadline,
    ) -> io::Result<i32> {
        Served {
            deadline: Some(deadline),
            ..self.served(values, Liveness::from_env())
        }
        .run(delivery)
    }

    /// rsync's far side, fed this process's stdin; the exit is the far side's.
    /// The tree the machine lays out first, when the call has one, is written to `prepared`.
    pub fn transfer(
        &self,
        values: &CallValues,
        live: Liveness,
        prepared: Option<&Path>,
    ) -> io::Result<i32> {
        self.served(values, live).transfer(prepared)
    }

    /// Starts the machine's side of a hold.
    pub fn hold(&self, values: &CallValues, live: Liveness) -> Holder {
        Served::holder(self.clone(), values.clone(), live)
    }

    /// Runs a call and keeps what it prints, stopping it when a bound passes first.
    pub fn ask(
        &self,
        values: &CallValues,
        bound: Option<Duration>,
        kept: Kept,
    ) -> io::Result<Answer> {
        Served {
            deadline: bound.map(Deadline::after),
            ..self.served(values, Liveness::from_env())
        }
        .ask(kept)
    }
}
