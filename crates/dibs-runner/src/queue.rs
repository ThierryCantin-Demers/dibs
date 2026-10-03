use crate::{
    call::Call,
    clock::{Moment, Span},
    history::{Estimate, History, Key, Scope},
    lock::{Kind, LockDir},
    platform::{Host, Platform as _},
    settings::Settings,
};
use dibs_format::{LockRecord, Mode};

/// The lock directory as a caller that is about to wait sees it.
pub struct Queue<'a> {
    pub dir: &'a LockDir,
    pub history: &'a History,
    pub call: &'a Call,
}

/// When the jobs waiting now start, worked out in arrival order. Queued shared jobs do not stand
/// in line behind one another: the shared lock admits them together, so the queue only advances
/// at a benchmark, which waits for everything admitted before it.
struct Eta {
    /// Seconds until the lock frees for the next benchmark, when `known`.
    at: u64,
    known: bool,
    /// The longest of the shared jobs admitted since, which a benchmark waits out too.
    pending: u64,
    pending_known: bool,
}

impl Eta {
    /// When a job of this mode and estimate would start; None when it cannot be said.
    fn next(&mut self, mode: Mode, estimate: Option<Estimate>) -> Option<u64> {
        if mode != Mode::Bench {
            match estimate {
                Some(e) => self.pending = self.pending.max(e.median),
                None => self.pending_known = false,
            }
            return self.known.then_some(self.at);
        }
        let eta = match self.known && self.pending_known {
            true => {
                let eta = self.at + self.pending;
                match estimate {
                    Some(e) => self.at = eta + e.median,
                    None => self.known = false,
                }
                Some(eta)
            }
            false => {
                self.known = false;
                None
            }
        };
        self.pending = 0;
        self.pending_known = true;
        eta
    }
}

impl Queue<'_> {
    fn estimate(&self, record: &LockRecord) -> Option<Estimate> {
        self.history.estimate(Key {
            mode: record.mode,
            label: &record.label,
            agent: Some(&record.agent),
            fingerprint: None,
        })
    }

    /// What a caller that has just been queued is told, in one line: what holds the machine, how
    /// many wait ahead of it and when it should start.
    pub fn line(&self) -> String {
        self.dir.prune();
        let now = Moment::epoch_now();
        let holders = self.dir.records(Kind::Holder);
        let mut free = 0;
        let mut known = true;
        for holder in &holders {
            match self
                .estimate(holder)
                .and_then(|e| e.remaining(now.saturating_sub(holder.start)))
            {
                Some(left) => free = free.max(left),
                None => known = false,
            }
        }
        let mut waiting = self.dir.records(Kind::Waiting);
        waiting.sort_by_key(|w| w.start);
        let mut eta = Eta {
            at: free,
            known,
            pending: 0,
            pending_known: true,
        };
        let mut ahead = 0;
        let mut mine = None;
        for waiter in &waiting {
            let starts = eta.next(waiter.mode, self.estimate(waiter));
            if waiter.pid == self.call.pid {
                mine = starts;
                break;
            }
            ahead += 1;
        }
        if holders.is_empty() {
            let orphans = self.orphans();
            if !orphans.is_empty() {
                let pids: String = orphans.iter().map(|p| format!(" {p}")).collect();
                return format!(
                    "dibs: the lock is held by an orphan (pid{pids}), which left no record, so nothing here can start. Reclaim it with: dibs --release\n"
                );
            }
        }
        let first = holders.first().map(|h| match h.mode {
            Mode::Bench => format!("the benchmark {}", h.label),
            _ => h.label.to_string(),
        });
        let mut line = format!(
            "dibs: queued and has not started, behind {}",
            first.as_deref().unwrap_or("a job starting up")
        );
        if holders.len() > 1 {
            line.push_str(&format!(" and {} more", holders.len() - 1));
        }
        if ahead > 0 {
            line.push_str(&format!(", with {ahead} queued first"));
        }
        if let Some(eta) = mine.filter(|e| *e > 0) {
            line.push_str(&format!(", ~{} until it starts", Span(eta)));
        }
        format!("{line}. dibs status shows the queue.\n")
    }

    /// Processes holding `rw` that nothing on record accounts for, outside this call's own group.
    fn orphans(&self) -> Vec<u32> {
        let mine = Host::group_of(self.call.pid);
        Host::lock_holders(&self.dir.rw())
            .into_iter()
            .filter(|&pid| Host::exists(pid))
            .filter(|&pid| mine.is_none() || Host::group_of(pid) != mine)
            .filter(|&pid| {
                !self.dir.file("holder", pid).exists() && !self.dir.file("waiting", pid).exists()
            })
            .collect()
    }

    /// A quick shared job may go around a queued benchmark, while that has waited less than the
    /// machine's patience and only when its own history says it will be gone within `quick`. It
    /// decides who waits, nothing else: a benchmark holding the lock refuses every shared caller.
    pub fn may_bypass(&self, settings: &Settings) -> bool {
        if self.call.mode() != Mode::Shared || !settings.bypass {
            return false;
        }
        let now = Moment::epoch_now();
        let benchmarks: Vec<u64> = self
            .dir
            .records(Kind::Waiting)
            .into_iter()
            .filter(|w| w.mode == Mode::Bench && Host::exists(w.pid))
            .map(|w| w.start)
            .collect();
        if benchmarks.is_empty()
            || benchmarks
                .iter()
                .any(|start| now.saturating_sub(*start) >= settings.patience)
        {
            return false;
        }
        self.history
            .estimate(Key {
                mode: self.call.mode(),
                label: self.call.label(),
                agent: Some(&self.call.agent),
                fingerprint: self.call.request.fingerprint.as_deref(),
            })
            .is_some_and(|e| e.scope == Scope::This && e.median <= settings.quick)
    }
}
