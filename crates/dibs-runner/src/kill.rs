//! `dibs --kill` and `dibs --release`, which take no lock: they are how a machine whose lock is
//! what is broken gets freed.

use crate::{
    call::{Journal, Received},
    clock::{Deadline, Moment, Span},
    job::{reap, tree_below},
    lock::{Kind, Lock, LockDir, RecordFile},
    platform::{Host, Platform as _},
    sink::Sink,
    status::Look,
};
use dibs_format::{Event, Exit, JobMeta, Label, LockRecord, Mode};
use std::{fs, thread, time::Duration};

/// How long apart the two readings of an orphaned lock are: a client tests the lock by taking it
/// and letting go at once, and killing a passer-by is worse than the wedge.
const SECOND_READING: Duration = Duration::from_millis(200);
/// An orphan's description is cut to this many characters, its indent included.
const DESCRIBED: usize = 100;
/// How long what a job left running is given to end after TERM before KILL.
const LEFTOVER_GRACE: Duration = Duration::from_secs(5);
/// How often it is looked at meanwhile.
const ROUND: Duration = Duration::from_millis(250);

/// A call that stops jobs or frees the lock.
pub struct Kill<'a> {
    pub call: &'a Received,
    pub look: Look<'a>,
    pub sink: &'a Sink,
}

/// What `--kill` was given: `<pid or batch id>[.any]`.
struct Target<'a> {
    name: &'a str,
    anyone: bool,
}

impl Kill<'_> {
    fn dir(&self) -> &LockDir {
        self.look.dir
    }

    fn journal(&self) -> Journal<'_> {
        Journal {
            path: &self.look.machine.log,
        }
    }

    fn force(&self) -> bool {
        self.call.mode() == Mode::KillForce
    }

    fn signal(&self) -> libc::c_int {
        match self.force() {
            true => libc::SIGKILL,
            false => libc::SIGTERM,
        }
    }

    /// The status as text, which a refusal shows.
    fn shown(&self) -> String {
        self.look.text(
            &self.look.status(self.call.request.verbose),
            self.call.request.tty,
        )
    }

    /// Whose job this is, checked before anything is signalled. An id that names an account, not
    /// a session, proves nothing, and a record with no id is not grounds to refuse.
    fn refused(&self, theirs: Option<&str>, anyone: bool) -> Option<Refusal> {
        match theirs.unwrap_or_default() {
            _ if anyone => None,
            id if id.starts_with("shell-") => Some(Refusal::Account),
            "" => None,
            id if !self.call.agent_id.is_empty() && id != self.call.agent_id => {
                Some(Refusal::Someone)
            }
            _ => None,
        }
    }

    pub fn serve(&self) -> i32 {
        let label = self.call.label().as_str();
        let target = Target {
            name: label.split('.').next().unwrap_or_default(),
            anyone: label.split_once('.').is_some_and(|(_, rest)| rest == "any"),
        };
        match batch_id(target.name) {
            true => self.batch(&target),
            false => self.pid(&target),
        }
    }

    fn pid(&self, target: &Target) -> i32 {
        let found = target.name.parse::<u32>().ok().and_then(|pid| {
            ["waiting", "holder"]
                .into_iter()
                .map(|kind| self.dir().file(kind, pid))
                .find(|file| file.exists())
        });
        let record = found.as_ref().and_then(|file| {
            fs::read_to_string(file)
                .ok()?
                .lines()
                .next()?
                .parse::<LockRecord>()
                .ok()
        });
        let (Some(file), Some(record)) = (found, record) else {
            if let Ok(pid) = target.name.parse::<u32>() {
                return self.leftover(pid, target.anyone);
            }
            self.sink.say(&format!(
                "Nothing holding or queued with pid {}.\n{}",
                target.name,
                self.shown()
            ));
            return Exit::Failed.status();
        };
        let pid = record.pid;
        let held = Span(Moment::epoch_now().saturating_sub(record.start));
        if !RecordFile(&file).written_by(pid) {
            let _ = fs::remove_file(&file);
            self.sink.say(&format!(
                "dibs: {} {} ended without clearing its record, and pid {pid} belongs to\n  \
                 something else now. The record is gone and nothing was signalled.\n",
                record.mode, record.label
            ));
            return Exit::Failed.status();
        }
        match self.refused(record.agent_id.as_deref(), target.anyone) {
            Some(Refusal::Someone) => {
                self.sink.say(&format!(
                    "dibs: {} {} (pid {pid}) belongs to {}, not to you.\n  \
                     It has been running {held}. If it is stuck and in your way, or you\n  \
                     know it should stop, say so:  dibs --kill {pid} --anyone\n",
                    record.mode, record.label, record.agent
                ));
                return Exit::Refused.status();
            }
            Some(Refusal::Account) => {
                self.sink.say(&format!(
                    "dibs: {} {} (pid {pid}) was started by {}, which names an account, not a\n  \
                     session, so dibs cannot tell whether it is yours. If you know it is, or that it\n  \
                     should stop:  dibs --kill {pid} --anyone\n  \
                     Export DIBS_AGENT once per session and your jobs can be told apart.\n",
                    record.mode,
                    record.label,
                    record.agent_id.as_deref().unwrap_or_default()
                ));
                return Exit::Refused.status();
            }
            None => {}
        }
        // TERM goes to the holder alone, which stops its own tree before it lets the lock go.
        let mut tree = vec![pid];
        if self.force() {
            tree.extend(tree_below(pid, &Host::processes()));
        }
        let signalled: Vec<String> = tree
            .iter()
            .rev()
            .filter(|&&victim| {
                // SAFETY: kill only sends a signal.
                unsafe { libc::kill(victim as libc::pid_t, self.signal()) == 0 }
            })
            .map(u32::to_string)
            .collect();
        let mut said = String::new();
        match signalled.is_empty() {
            true => self.sink.say(&format!(
                "Could not signal pid {pid}; it may already be gone.\n"
            )),
            false => {
                let mut line = self.call.log_line(Event::Killed);
                line.command = format!(
                    "killed {} {} (pid {pid}) after {held}: {}",
                    record.mode, record.label, record.command
                );
                self.journal().write(&line);
                said.push_str(&format!(
                    "Sent SIG{} to {} ({} {}, held {held}).\nIt belonged to {}.\n",
                    match self.force() {
                        true => "KILL",
                        false => "TERM",
                    },
                    signalled.join(" "),
                    record.mode,
                    record.label,
                    record.agent
                ));
            }
        }
        if !self.force() {
            said.push_str(&format!(
                "If it survives that, run: dibs --kill {pid} --force\n"
            ));
        }
        self.sink.out(said.as_bytes());
        0
    }

    /// A process that outlived its job holds no lock, so no record names it; its environment
    /// still names the job, and the job's meta whose it was. It goes with what it started.
    fn leftover(&self, pid: u32, anyone: bool) -> i32 {
        let job = Host::variable(pid, "DIBS_JOB")
            .filter(|job| !job.is_empty() && job.bytes().all(|b| b.is_ascii_digit() || b == b'-'));
        let Some(job) = job else {
            self.sink.say(&format!(
                "dibs: nothing was stopped: pid {pid} holds no lock here, waits for none, and no dibs job started it.\n{}",
                self.shown()
            ));
            return Exit::Failed.status();
        };
        let meta = fs::read_to_string(self.look.machine.jobs().join(&job).join("meta"))
            .ok()
            .and_then(|text| text.parse::<JobMeta>().ok());
        let Some(meta) = meta else {
            self.sink.say(&format!(
                "dibs: nothing was stopped: pid {pid} belongs to job {job}, which is still running. Stop the job:\n  \
                 dibs --kill <its pid in dibs status>\n"
            ));
            return Exit::Failed.status();
        };
        let label = &meta.label;
        if self.refused(meta.who.as_deref(), anyone).is_some() {
            self.sink.say(&format!(
                "dibs: pid {pid} was left running by job {job} ({label}), which belonged to {}.\n  \
                 If you know it should stop:  dibs --kill {pid} --anyone\n",
                meta.agent
            ));
            return Exit::Refused.status();
        }
        let mut tree = vec![pid];
        tree.extend(tree_below(pid, &Host::processes()));
        let send = |signal| {
            for victim in tree.iter().rev() {
                // SAFETY: kill only sends a signal.
                unsafe { libc::kill(*victim as libc::pid_t, signal) };
            }
        };
        send(libc::SIGTERM);
        if !Deadline::after(Some(LEFTOVER_GRACE))
            .until(ROUND, || tree.iter().all(|p| !Host::running(*p)))
        {
            send(libc::SIGKILL);
        }
        let mut line = self.call.log_line(Event::Killed);
        line.command = format!("killed what job {job} ({label}) left running: pid {pid}");
        self.journal().write(&line);
        self.sink.out(
            format!(
                "Stopped pid {pid} and what it started, left running by job {job} ({label}) after the job had ended.\n"
            )
            .as_bytes(),
        );
        0
    }

    /// Every job of a batch here: a holder loses what runs under it and exits 76 itself, so the
    /// lock goes the ordinary way; a waiter is stopped outright, since its lock would otherwise
    /// come and its command run. The mark refuses the batch's later steps here for a day, which
    /// is what stops a driver on another computer.
    fn batch(&self, target: &Target) -> i32 {
        let id = target.name;
        let jobs: Vec<u32> = self
            .dir()
            .named("batch")
            .into_iter()
            .filter(|file| {
                fs::read_to_string(file)
                    .ok()
                    .and_then(|plan| Some(plan.lines().next()?.split('\t').next()? == id))
                    .unwrap_or(false)
            })
            .filter_map(|file| RecordFile(&file).pid())
            .collect();
        let steps: Vec<Step> = jobs
            .iter()
            .filter_map(|&pid| {
                [Kind::Holder, Kind::Waiting].into_iter().find_map(|kind| {
                    let file = match kind {
                        Kind::Holder => self.dir().file("holder", pid),
                        Kind::Waiting => self.dir().file("waiting", pid),
                    };
                    let text = fs::read_to_string(file).ok()?;
                    Some(Step {
                        pid,
                        kind,
                        record: text.lines().next()?.parse().ok()?,
                    })
                })
            })
            .collect();
        if let Some(Step { record, .. }) = steps.iter().find(|step| {
            self.refused(step.record.agent_id.as_deref(), target.anyone)
                .is_some()
        }) {
            self.sink.say(&format!(
                "dibs: batch {id} is running {} for {}, not for you. If it should stop:\n  \
                 dibs --kill {id} --anyone\n",
                record.label, record.agent
            ));
            return Exit::Refused.status();
        }
        let _ = fs::write(self.dir().path.join(format!("cancelled.{id}")), "");
        let mut line = self.call.log_line(Event::Cancelled);
        line.label = Label::new(id);
        line.command = format!("cancelled batch {id}: {} job(s) stopped", steps.len());
        self.journal().write(&line);
        let processes = Host::processes();
        for step in &steps {
            let mut victims = match step.kind {
                Kind::Holder => Vec::new(),
                Kind::Waiting => vec![step.pid],
            };
            victims.extend(tree_below(step.pid, &processes));
            for victim in victims.iter().rev() {
                // SAFETY: kill only sends a signal.
                unsafe { libc::kill(*victim as libc::pid_t, self.signal()) };
            }
        }
        self.sink.out(
            format!(
                "Cancelled batch {id} on {}: stopped {} job(s), and its later steps are refused here.\n",
                self.look.machine.host,
                steps.len()
            )
            .as_bytes(),
        );
        0
    }

    /// Prunes what dead jobs left, ends an orphan holding the lock, and shows what is left.
    pub fn release(&self) -> i32 {
        self.dir().prune();
        let mut said = String::from("Pruned dead entries.\n");
        if !Lock::untaken(self.dir()) {
            said.push_str(&self.reclaim());
        }
        said.push_str(&self.shown());
        said.push_str("A live holder is a running command. Stop it with: kill <pid>\n");
        self.sink.out(said.as_bytes());
        0
    }

    /// An orphan holds the lock through a descriptor, so there is nothing to unlink: freeing the
    /// machine means ending the process, and only one seen in two readings a moment apart.
    fn reclaim(&self) -> String {
        let first = self.dir().takers(self.call.pid).orphans;
        if first.is_empty() {
            return String::new();
        }
        thread::sleep(SECOND_READING);
        let kept: Vec<u32> = self
            .dir()
            .takers(self.call.pid)
            .orphans
            .into_iter()
            .filter(|pid| first.contains(pid))
            .collect();
        if kept.is_empty() {
            return String::new();
        }
        let mut said =
            String::from("The lock was held by an orphan, which left no record. Reclaiming it:\n");
        for &pid in &kept {
            if let Some(described) = Host::describe(pid) {
                let line = format!("  {described}");
                said.push_str(&line.chars().take(DESCRIBED).collect::<String>());
                said.push('\n');
            }
        }
        let pids: String = kept.iter().map(|p| format!(" {p}")).collect();
        let mut line = self.call.log_line(Event::Reclaimed);
        line.label = Label::new("reclaim");
        line.command = format!("reclaimed the lock from orphan pid{pids}");
        self.journal().write(&line);
        reap(&kept);
        for pid in kept.iter().filter(|&&pid| Host::running(pid)) {
            self.sink.say(&format!(
                "  pid {pid} survived; run it again, or kill -9 {pid}\n"
            ));
        }
        said
    }
}

/// A job of a batch being cancelled, and how it is found here.
struct Step {
    pid: u32,
    kind: Kind,
    record: LockRecord,
}

/// Why a kill is refused before anything is signalled.
enum Refusal {
    /// Another session's job.
    Someone,
    /// Started by an id that names an account, which every shell of it shares.
    Account,
}

/// `YYYYMMDD-HHMMSS-N`, a batch's id rather than a pid.
fn batch_id(target: &str) -> bool {
    let parts: Vec<&str> = target.split('-').collect();
    matches!(parts.as_slice(), [day, time, n]
        if day.len() == 8 && time.len() == 6 && !n.is_empty()
            && [day, time, n].iter().all(|p| p.bytes().all(|b| b.is_ascii_digit())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_id_is_told_from_a_pid() {
        assert!(batch_id("20261001-120000-77"));
        assert!(!batch_id("4242"));
        assert!(!batch_id("20261001120000-4242"));
        assert!(!batch_id("2026100-120000-77"));
    }
}
