use crate::{
    clock::Moment,
    closed::Shutter,
    history::{Estimate, History, Key, Scope},
    job::Tree,
    lock::{Kind, Lock, LockDir},
    machine::Site,
    platform::{Host, Platform as _},
    queue::Eta,
    settings::{Settings, home},
    status::{
        batch::{Plan, Tail},
        cpu,
        scene::{Leftover, Listing, Orphan, Scene, Service},
        text::Snapshot,
    },
};
use dibs_format::{
    JobId, JobMeta, LockRecord, Mode,
    status::{
        BatchShown, Holder, HolderState, IdleWait, Idleness, JobCall, LockState, LowPriorityStop,
        PreemptionState, RemainingBasis, ShownEstimate, Silence, Status, Waiter,
    },
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read as _, Seek as _, SeekFrom},
    num::NonZero,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// A look at the machine's lock, from its records and what the system says of the processes
/// they name. Nothing here forks.
#[derive(Clone, Copy)]
pub struct Look<'a> {
    pub machine: &'a Site,
    pub dir: &'a LockDir,
    pub history: &'a History,
    pub settings: &'a Settings,
    /// The process looking, whose own group holding the lock is not an orphan.
    pub asking: u32,
}

impl Look<'_> {
    /// The status as it stands; with `listed`, the lock directory's entries too.
    ///
    /// # Guarantees
    ///
    /// - `state` is `Bench` or `Shared` exactly when `holders` is not empty.
    /// - A holder's `idle` is set only once its tree has used no CPU for longer than
    ///   `idle_after`, and only where [`HolderState::of`] then says it is idle: never while it is
    ///   urgent with the machine to itself, parked, frozen or preparing.
    /// - A waiter's `idle_wait` is Some exactly when its `low_priority` is set.
    pub fn snapshot(&self, listed: bool) -> Snapshot {
        self.dir.prune();
        let records = self.dir.records(Kind::Holder);
        let mut waiting = self.dir.records(Kind::Waiting);
        waiting.sort_by_key(|w| w.start_secs);
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
            leftovers: self.leftovers(),
            services: records
                .iter()
                .map(|record| (record.pid, self.services(record.pid)))
                .collect::<BTreeMap<_, _>>(),
            ..Scene::default()
        };
        let bench = records.iter().any(|r| r.mode == Mode::Bench);
        let state = match records.first().map(|r| r.mode) {
            Some(_) if bench => LockState::Bench,
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
        let status = Status {
            read_secs: now,
            state,
            core_count: std::thread::available_parallelism().map_or(1, NonZero::get) as u64,
            load_hundredths: load(),
            caches: caches(&self.machine.scratch.join("target")),
            clones: clones(&home().join("prog")),
            holders,
            queue,
            closed: (Shutter { dir: self.dir }).read(),
            metrics: None,
            series: crate::series::here(&self.machine.history.with_file_name("cards")),
        };
        Snapshot { status, scene }
    }

    /// Processes of this account whose environment names a job that has written its meta, which
    /// a job does as it ends.
    fn leftovers(&self) -> Vec<Leftover> {
        let jobs = self.machine.jobs();
        Host::processes()
            .iter()
            .filter_map(|process| {
                let job = JobId::checked_from_text(&Host::variable(process.pid, "DIBS_JOB")?)?;
                let meta = fs::read_to_string(jobs.join(job.as_str()).join("meta")).ok()?;
                let meta: JobMeta = meta.parse().ok()?;
                Some(Leftover {
                    job,
                    label: meta.label,
                    described: Host::describe(process.pid)
                        .unwrap_or_else(|| process.pid.to_string()),
                })
            })
            .collect()
    }

    /// The status as text, coloured when the caller's stdout is a terminal.
    pub fn text(&self, snapshot: &Snapshot, tty: bool) -> String {
        snapshot.to_text(tty && crate::settings::var("NO_COLOR").is_none())
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
        let queued: Vec<&LockRecord> = waiting
            .iter()
            .filter(|w| !self.dir.idle_only(w.pid))
            .collect();
        for waiter in &queued {
            eta.next(waiter.mode, self.estimate(waiter, None));
        }
        Tail {
            eta,
            exclusive: holders.iter().chain(queued).any(|r| r.mode == Mode::Bench),
        }
    }

    fn holder(&self, record: &LockRecord, now: u64, tail: &Tail, free: &mut Eta) -> Holder {
        let pid = record.pid;
        let elapsed = now.saturating_sub(record.start_secs);
        let tree = Tree::of(pid);
        let ticks = tree.work_ticks();
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
            start: record.start_secs,
        }
        .sample();
        let unseen = self.dir.works_unseen(pid);
        let written = output.as_deref().and_then(|file| age(Path::new(file)));
        let writing = written.is_some_and(|age| (age as i64) < self.settings.wrote_within);
        let estimate = self.estimate(record, record.fingerprint.as_deref());
        let shown = estimate.map(|e| shown(e, elapsed));
        let prepare = self.dir.prepare_of(pid);
        // A label whose own runs take this long may be quiet by nature, as a benchmark that
        // prints once at the end is.
        let usual = shown
            .as_ref()
            .is_some_and(|s| s.scope == Scope::This && s.remaining.is_some());
        let silent = output
            .as_deref()
            .zip(written)
            .filter(|&(_, age)| age as i64 > self.settings.silent_after)
            .filter(|_| !usual && prepare.is_none())
            .map(|(file, silent_for_secs)| Silence {
                silent_for_secs,
                silent_line: last_line(Path::new(file)),
            });
        let left = estimate.and_then(|e| e.remaining(elapsed));
        // A job run only on an idle machine is parked for whoever comes, so it frees it at once.
        match left {
            _ if self.dir.idle_only(pid) => {}
            Some(left) => free.at = free.at.max(left),
            None => free.known = false,
        }
        let mut holder = Holder {
            call: self.call(record),
            job: record.job.clone(),
            started_secs: record.start_secs,
            elapsed_secs: elapsed,
            killed_at: self.killed_at(pid, record.start_secs),
            cpu_secs: ticks / Host::clock_ticks(),
            estimate: shown,
            output,
            cpu_rate: sample.rate,
            idle: sample.idle.filter(|_| !writing && !unseen),
            silent,
            batch: self.batch(pid, left, tail),
            prepare,
            ahead: self.dir.ahead_of(pid),
            urgent: self.dir.urgent_why(pid),
            parked: self.dir.parked_for(pid),
            low_priority: self.dir.idle_only(pid),
        };
        holder.idle = idle_shown(&holder, self.settings.idle_after);
        holder
    }

    /// The call a lock record and the files beside it say a job was started by.
    fn call(&self, record: &LockRecord) -> JobCall {
        JobCall {
            mode: record.mode,
            pid: record.pid,
            label: record.label.clone(),
            agent: record.agent.clone(),
            user: self.user(record.pid),
            subagent: self.subagent(record.pid),
            device: record.device.clone(),
            cmd: record.command.clone(),
            command: self.command(record.pid),
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
        let low_priority = self.dir.idle_only(record.pid);
        let starts = match low_priority {
            true => None,
            false => eta.next(record.mode, estimate),
        };
        let left = starts.and_then(|at| {
            estimate
                .filter(|e| e.scope == Scope::This)
                .map(|e| at + e.median)
        });
        Waiter {
            position,
            call: self.call(record),
            arrived_secs: record.start_secs,
            waiting_secs: now.saturating_sub(record.start_secs),
            eta: starts,
            low_priority,
            urgent: self.dir.urgent_why(record.pid),
            idle_wait: low_priority.then(|| {
                let quiet = self.dir.quiet(record.pid);
                IdleWait {
                    behind: self
                        .dir
                        .busy_besides(record.pid)
                        .map(|busy| format!("{} from {}", busy.label, busy.agent)),
                    quiet_for: quiet
                        .and_then(|(since, _)| since)
                        .map(|since| now.saturating_sub(since)),
                    settle_secs: quiet.map_or(self.settings.idle_settle, |(_, settle)| settle),
                    stopped: self
                        .dir
                        .stopped(record.pid)
                        .map(|(ran, at, by)| LowPriorityStop {
                            ran_secs: ran,
                            ago_secs: now.saturating_sub(at),
                            by,
                        }),
                }
            }),
            batch: self.batch(record.pid, left, tail),
        }
    }

    fn user(&self, pid: u32) -> Option<String> {
        let said = fs::read_to_string(self.dir.file("user", pid)).ok()?;
        Some(said.trim().to_string()).filter(|s| !s.is_empty())
    }

    /// On the clock `elapsed` counts by, whole seconds since the epoch. None while frozen or
    /// parked, whose ends move the deadline.
    fn killed_at(&self, pid: u32, start: u64) -> Option<u64> {
        let waiting = matches!(self.dir.ahead_of(pid), Some(PreemptionState::Paused(_)))
            || self.dir.parked_for(pid).is_some();
        let at = self.dir.deadline(pid).filter(|_| !waiting)?;
        Some((at.floor() as u64).saturating_sub(start))
    }

    fn command(&self, pid: u32) -> Option<String> {
        fs::read_to_string(self.dir.file("command", pid)).ok()
    }

    fn subagent(&self, pid: u32) -> Option<String> {
        let said = fs::read_to_string(self.dir.file("subagent", pid)).ok()?;
        Some(said.trim().to_string()).filter(|s| !s.is_empty())
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
            run_count: fs::read_to_string(&self.machine.history)
                .map_or(0, |text| text.lines().count()),
        }
    }
}

/// The holder's idleness as its document says it: past `idle_after_secs`, and only where every
/// view would say it, so none while it is urgent with the machine to itself, waits or prepares.
fn idle_shown(holder: &Holder, idle_after_secs: i64) -> Option<Idleness> {
    holder
        .idle
        .clone()
        .filter(|idle| idle.idle_for_secs as i64 > idle_after_secs)
        .filter(|_| matches!(HolderState::of(holder), HolderState::Idle(_)))
}

/// An estimate as the status shows it, measured against how long the job has run.
fn shown(estimate: Estimate, elapsed: u64) -> ShownEstimate {
    let remaining = estimate.remaining(elapsed);
    ShownEstimate {
        median_secs: estimate.median,
        low_secs: estimate.low,
        high_secs: estimate.high,
        run_count: estimate.runs,
        scope: estimate.scope,
        wide: estimate.runs >= 4 && estimate.high > (estimate.low + 1) * 3,
        other: estimate.other,
        remaining,
        remaining_kind: remaining.map(|_| match elapsed < estimate.median {
            true => RemainingBasis::Typical,
            false => RemainingBasis::Bound,
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

/// The last line in the file's last few kilobytes, a progress bar's last redraw included.
fn last_line(file: &Path) -> String {
    const TAIL: u64 = 4096;
    const WIDTH: usize = 160;
    let mut bytes = Vec::new();
    if let Ok(mut file) = File::open(file) {
        let len = file.metadata().map_or(0, |m| m.len());
        let _ = file.seek(SeekFrom::Start(len.saturating_sub(TAIL)));
        let _ = file.take(TAIL).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes)
        .split(['\n', '\r'])
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(|line| {
            line.chars()
                .filter(|c| !c.is_control())
                .take(WIDTH)
                .collect()
        })
        .unwrap_or_default()
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
    use dibs_format::{Label, status::IdleKind};

    const IDLE_AFTER_SECS: i64 = 60;

    fn holder(mode: Mode) -> Holder {
        Holder {
            call: JobCall {
                mode,
                pid: 7,
                label: Label::new("b"),
                agent: "a".into(),
                user: None,
                subagent: None,
                device: None,
                cmd: "c".into(),
                command: None,
            },
            job: None,
            started_secs: 0,
            elapsed_secs: 600,
            killed_at: None,
            cpu_secs: 0,
            estimate: None,
            output: None,
            cpu_rate: None,
            idle: None,
            silent: None,
            batch: None,
            prepare: None,
            ahead: None,
            urgent: None,
            parked: None,
            low_priority: false,
        }
    }

    /// What the document says of `holder`'s idleness once its tree has been idle `secs`.
    fn shown_idle_for(mut holder: Holder, secs: u64) -> Option<Idleness> {
        holder.idle = Some(Idleness {
            idle_for_secs: secs,
            idle_kind: IdleKind::Never,
        });
        idle_shown(&holder, IDLE_AFTER_SECS)
    }

    #[test]
    fn a_holder_idle_past_the_threshold_is_called_idle() {
        assert!(shown_idle_for(holder(Mode::Shared), 61).is_some());
    }

    #[test]
    fn a_holder_idle_no_longer_than_the_threshold_is_not() {
        assert_eq!(shown_idle_for(holder(Mode::Shared), 60), None);
    }

    #[test]
    fn a_preparing_holder_is_not_called_idle() {
        let mut preparing = holder(Mode::Shared);
        preparing.prepare = Some(String::new());
        assert_eq!(shown_idle_for(preparing, 600), None);
    }

    #[test]
    fn a_holder_parked_for_an_urgent_job_is_not_called_idle() {
        let mut parked = holder(Mode::Shared);
        parked.parked = Some(JobId::new("20261001120000-9"));
        assert_eq!(shown_idle_for(parked, 600), None);
    }

    #[test]
    fn a_holder_frozen_for_a_benchmark_is_not_called_idle() {
        let mut frozen = holder(Mode::Shared);
        frozen.ahead = Some(PreemptionState::Paused(JobId::new("20261001120000-9")));
        assert_eq!(shown_idle_for(frozen, 600), None);
    }

    #[test]
    fn an_urgent_benchmark_is_not_called_idle() {
        let mut urgent = holder(Mode::Bench);
        urgent.urgent = Some("demo".into());
        assert_eq!(shown_idle_for(urgent, 600), None);
    }

    #[test]
    fn an_urgent_shared_job_is_still_called_idle() {
        let mut urgent = holder(Mode::Shared);
        urgent.urgent = Some("demo".into());
        assert!(shown_idle_for(urgent, 600).is_some());
    }

    #[test]
    fn only_a_services_own_log_is_left_out_of_a_holders_output() {
        assert!(service_log(Path::new("/s/jobs/j/with-srv_1.log")));
        assert!(!service_log(Path::new("/s/out/with-Srv.log")));
        assert!(!service_log(Path::new("/s/out/build.log")));
    }
}
