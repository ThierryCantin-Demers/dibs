use crate::{
    job::{Digest, Repeat, built},
    machine::Machine,
    session::{base::Session, run::Hosted},
};
use dibs_format::{
    By, JobId, JobMeta,
    wire::{Built, Record, Trailer},
};
use std::{fs, path::PathBuf};

/// A job that has ended, which its caller is told about.
pub(super) struct Ended<'a> {
    pub(super) session: &'a Session,
    pub(super) machine: &'a Machine,
    pub(super) job: &'a JobId,
    pub(super) log: &'a PathBuf,
    pub(super) lines: usize,
    pub(super) job_dir: &'a PathBuf,
    pub(super) hosted: &'a Hosted,
    pub(super) waited: u64,
    pub(super) ran: u64,
    pub(super) status: i32,
    pub(super) by: By,
}

impl Ended<'_> {
    /// What `dibs out` reads back about the job, beside its log.
    pub(super) fn keep(&self) {
        let call = &self.session.call;
        let meta = JobMeta {
            mode: call.mode(),
            label: call.label().clone(),
            queued: self.waited,
            ran: self.ran,
            exit: self.status,
            by: self.by,
            agent: call.agent.clone(),
            lines: self.lines as u64,
        };
        let _ = fs::write(self.job_dir.join("meta"), meta.to_string());
    }

    /// The digest, then the trailer and what follows it: the same shape every time, on stderr,
    /// where a pipe on the caller's side cannot cut it off.
    pub(super) fn report(&self) {
        let session = self.session;
        let call = &session.call;
        let host = &self.machine.host;
        let lines = self.lines;
        let text = fs::read(self.log).unwrap_or_default();
        if !call.request.stream {
            session.sink.out(
                &Digest {
                    log: &text,
                    head: session.settings.digest_head,
                    tail: session.settings.digest_tail,
                    host,
                    job: self.job,
                    path: self.log,
                }
                .text(),
            );
        }
        let built = built(&text);
        session.sink.record(Record::Trailer(Trailer {
            job: self.job.clone(),
            mode: call.mode(),
            label: call.label().clone(),
            queued: self.waited,
            ran: self.ran,
            exit: self.status,
            by: self.by,
            built,
        }));
        let mut after = String::new();
        if !call.request.watch.hold {
            after.push_str(&format!(
                "  log {host}:{}  ({lines} lines)  dibs --on {host} --out {}\n",
                self.log.display(),
                self.job
            ));
        }
        after.push_str(&self.hosted.lines(host));
        if built == Some(Built::Nothing) {
            after.push_str(
                "  built nothing: cargo compiled 0 crates, so a measurement after this measures the previous binary.\n",
            );
        }
        if self.status != 0
            && !call.request.watch.hold
            && let Some(earlier) = (Repeat {
                jobs: &self.machine.jobs(),
                this: self.job_dir,
                window: session.settings.repeat_window,
            })
            .earlier()
        {
            after.push_str(&format!(
                "  this exact command already failed here: job {earlier}. Unchanged, it failed the same way.\n"
            ));
        }
        session.sink.say(&after);
    }
}
