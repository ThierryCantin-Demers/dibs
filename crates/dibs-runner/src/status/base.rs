use crate::{
    clock::Moment,
    history::{Estimate, History, Key, Scope},
    job::Tree,
    lock::{Kind, Lock, LockDir},
    machine::Machine,
    platform::{Host, Platform as _},
    queue::Eta,
    settings::{Settings, home},
    status::{
        batch::{Plan, Tail},
        cpu,
    },
};
use dibs_format::{
    LockRecord, Mode,
    status::{
        BatchShown, Holder, Listing, LockState, Orphan, Remaining, Scene, Service, Shown, Status,
        Text, Waiter,
    },
};
use std::{
    fs,
    num::NonZero,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// A look at the machine's lock, from its records and what the system says of the processes
/// they name. Nothing here forks.
pub struct Look<'a> {
    pub machine: &'a Machine,
    pub dir: &'a LockDir,
    pub history: &'a History,
    pub settings: &'a Settings,
    /// The process looking, whose own group holding the lock is not an orphan.
    pub asking: u32,
}

impl Look<'_> {
    /// The status as it stands; with `listed`, the lock directory's entries too.
    pub fn status(&self, listed: bool) -> Status {
        self.dir.prune();
        let records = self.dir.records(Kind::Holder);
        let mut waiting = self.dir.records(Kind::Waiting);
        waiting.sort_by_key(|w| w.start);
        let now = Moment::epoch_now();
        let tail = self.tail(&records, &waiting, now);
        let mut free = Eta {
            at: 0,
            known: true,
            pending: 0,
            pending_known: true,
        };
        let holders: Vec<Holder> = records
            .iter()
            .map(|record| self.holder(record, now, &tail, &mut free))
            .collect();
        let queue = waiting
            .iter()
            .enumerate()
            .map(|(at, record)| self.waiter(at + 1, record, now, &tail, &mut free))
            .collect();
        let mut scene = Scene {
            host: self.machine.host.clone(),
            listing: listed.then(|| self.listing()),
            ..Scene::default()
        };
        let state = match records.first().map(|r| r.mode) {
            Some(Mode::Bench) => LockState::Bench,
            Some(_) => LockState::Shared,
            None if Lock::untaken(self.dir) => LockState::Idle,
            None => {
                let takers = self.dir.takers(self.asking);
                scene.unseen = !takers.seen;
                scene.orphans = takers
                    .orphans
                    .iter()
                    .map(|&pid| Orphan {
                        pid,
                        described: Host::describe(pid).unwrap_or_else(|| pid.to_string()),
                    })
                    .collect();
                match scene.orphans.is_empty() {
                    true => LockState::Busy,
                    false => LockState::Orphan,
                }
            }
        };
        Status {
            t: now,
            state,
            cores: std::thread::available_parallelism().map_or(1, NonZero::get) as u64,
            load: load(),
            caches: caches(&self.machine.scratch.join("target")),
            clones: clones(&home().join("prog")),
            holders,
            queue,
            scene,
        }
    }

    /// The status as text, coloured when the caller's stdout is a terminal.
    pub fn text(&self, status: &Status, tty: bool) -> String {
        Text {
            status,
            colour: tty && crate::settings::var("NO_COLOR").is_none(),
            idle_after: self.settings.idle_after,
        }
        .to_string()
    }

    fn estimate(&self, record: &LockRecord, fingerprint: Option<&str>) -> Option<Estimate> {
        self.history.estimate(Key {
            mode: record.mode,
            label: &record.label,
            agent: Some(&record.agent),
            fingerprint,
        })
    }

    /// Where the queue stands once all of it has started, which only a batch's later steps need.
    fn tail(&self, holders: &[LockRecord], waiting: &[LockRecord], now: u64) -> Tail {
        let mut eta = Eta::behind(holders, now, |h| self.estimate(h, None));
        for waiter in waiting {
            eta.next(waiter.mode, self.estimate(waiter, None));
        }
        Tail {
            eta,
            exclusive: holders.iter().chain(waiting).any(|r| r.mode == Mode::Bench),
        }
    }

    fn holder(&self, record: &LockRecord, now: u64, tail: &Tail, free: &mut Eta) -> Holder {
        let pid = record.pid;
        let elapsed = now.saturating_sub(record.start);
        let tree = Tree::of(pid);
        let ticks = tree.ticks();
        let output = tree
            .written()
            .into_iter()
            .find(|file| !service_log(file))
            .map(|file| file.display().to_string());
        let sample = cpu::Look {
            file: &self.dir.file("cpu", pid),
            ticks,
            clock_ticks: Host::clock_ticks(),
            now,
            start: record.start,
        }
        .sample();
        let held_elsewhere = self.dir.file("hold", pid).exists();
        let writing = output.as_deref().is_some_and(|file| {
            age(Path::new(file)).is_some_and(|age| (age as i64) < self.settings.wrote_within)
        });
        let idle = sample.idle.filter(|_| !writing && !held_elsewhere);
        let estimate = self.estimate(record, record.fingerprint.as_deref());
        let left = estimate.and_then(|e| e.remaining(elapsed));
        match left {
            Some(left) => free.at = free.at.max(left),
            None => free.known = false,
        }
        Holder {
            mode: record.mode,
            pid,
            job: record.job.clone(),
            label: record.label.clone(),
            agent: record.agent.clone(),
            device: record.device.clone(),
            cmd: record.command.clone(),
            started: record.start,
            elapsed,
            cpu: ticks / Host::clock_ticks(),
            estimate: estimate.map(|e| shown(e, elapsed)),
            output,
            cpu_rate: sample.rate,
            idle,
            batch: self.batch(pid, left, tail),
            services: self.services(pid),
        }
    }

    fn waiter(
        &self,
        position: usize,
        record: &LockRecord,
        now: u64,
        tail: &Tail,
        eta: &mut Eta,
    ) -> Waiter {
        let estimate = self.estimate(record, None);
        let starts = eta.next(record.mode, estimate);
        let left = starts.and_then(|at| {
            estimate
                .filter(|e| e.scope == Scope::This)
                .map(|e| at + e.median)
        });
        Waiter {
            position,
            mode: record.mode,
            pid: record.pid,
            label: record.label.clone(),
            agent: record.agent.clone(),
            device: record.device.clone(),
            cmd: record.command.clone(),
            arrived: record.start,
            waiting: now.saturating_sub(record.start),
            eta: starts,
            batch: self.batch(record.pid, left, tail),
        }
    }

    fn batch(&self, pid: u32, left: Option<u64>, tail: &Tail) -> Option<BatchShown> {
        let plan = fs::read_to_string(self.dir.file("batch", pid)).ok()?;
        Some(
            Plan {
                plan: plan.parse().ok()?,
                history: self.history,
            }
            .shown(left, tail),
        )
    }

    /// The `--with` servers a holder runs, as `with.<pid>` lists them.
    fn services(&self, pid: u32) -> Vec<Service> {
        fs::read_to_string(self.dir.file("with", pid))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\t');
                Some(Service {
                    name: fields.next()?.to_string(),
                    pid: fields.next()?.parse().ok()?,
                    command: fields.next().unwrap_or_default().to_string(),
                })
            })
            .collect()
    }

    fn listing(&self) -> Listing {
        let mut entries: Vec<String> = fs::read_dir(&self.dir.path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        Listing {
            dir: self.dir.path.clone(),
            entries,
            history: self.machine.history.clone(),
            runs: fs::read_to_string(&self.machine.history).map_or(0, |text| text.lines().count()),
        }
    }
}

/// An estimate as the status shows it, measured against how long the job has run.
fn shown(estimate: Estimate, elapsed: u64) -> Shown {
    let remaining = estimate.remaining(elapsed);
    Shown {
        median: estimate.median,
        low: estimate.low,
        high: estimate.high,
        runs: estimate.runs,
        scope: estimate.scope,
        wide: estimate.runs >= 4 && estimate.high > (estimate.low + 1) * 3,
        other: estimate.other,
        remaining,
        remaining_kind: remaining.map(|_| match elapsed < estimate.median {
            true => Remaining::Typical,
            false => Remaining::Bound,
        }),
        overrun: estimate.scope == Scope::This
            && !estimate.other
            && estimate.high > 0
            && elapsed > estimate.high * 2,
    }
}

/// A `--with` server's own log, which is not the job's output.
fn service_log(file: &Path) -> bool {
    file.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("with-")?.strip_suffix(".log"))
        .is_some_and(|name| {
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// Seconds since the file was written.
fn age(file: &Path) -> Option<u64> {
    let written = fs::metadata(file).ok()?.modified().ok()?;
    Some(
        SystemTime::now()
            .duration_since(written)
            .map_or(0, |d| d.as_secs()),
    )
}

/// The one-minute load average, times 100.
fn load() -> u64 {
    let mut averages = [0f64; 3];
    // SAFETY: getloadavg writes at most the 3 values the buffer holds.
    match unsafe { libc::getloadavg(averages.as_mut_ptr(), 3) } {
        1.. => (averages[0] * 100.0) as u64,
        _ => 0,
    }
}

/// The repos with a build cache here: a marker cargo writes, since preparing a tree makes its
/// target directory whether or not anything is ever built there.
fn caches(target: &Path) -> Vec<String> {
    named_dirs(target, |dir| {
        dir.join(".rustc_info.json").is_file()
            || dir.join("release").is_dir()
            || dir.join("debug").is_dir()
    })
}

/// The repos a worktree can be prepared from.
fn clones(prog: &Path) -> Vec<String> {
    named_dirs(prog, |dir| dir.join(".git").exists())
}

fn named_dirs(parent: &Path, keep: impl Fn(&PathBuf) -> bool) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|path| path.is_dir() && keep(path))
        .filter_map(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_services_own_log_is_left_out_of_a_holders_output() {
        assert!(service_log(Path::new("/s/jobs/j/with-srv_1.log")));
        assert!(!service_log(Path::new("/s/out/with-Srv.log")));
        assert!(!service_log(Path::new("/s/out/build.log")));
    }
}
