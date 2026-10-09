use crate::{
    job::{Digest, LogRead, Repeat},
    session::run::{Hosted, Serving},
};
use dibs_format::{
    By, JobId, JobMeta,
    wire::{Built, Record, Trailer},
};
use std::{fs, path::PathBuf};

/// What a job that held the lock came to: its log, how long it waited and ran, and its status.
pub struct Tally<'a> {
    pub job: &'a JobId,
    pub log: &'a PathBuf,
    /// Read once, under the lock, for `meta` and the trailer.
    pub read: LogRead,
    pub job_dir: &'a PathBuf,
    pub hosted: &'a Hosted,
    pub waited: u64,
    pub ran: u64,
    pub status: i32,
    pub by: By,
}

/// A job that has ended, which its caller is told about.
pub struct Ended<'a> {
    at: Serving<'a>,
    tally: Tally<'a>,
}

impl<'a> Ended<'a> {
    pub fn new(at: Serving<'a>, tally: Tally<'a>) -> Self {
        Ended { at, tally }
    }

    /// What `dibs out` reads back about the job, beside its log.
    pub fn keep(&self) {
        let call = self.at.call;
        let tally = &self.tally;
        let meta = JobMeta {
            mode: call.mode(),
            label: call.label().clone(),
            queued: tally.waited,
            ran: tally.ran,
            exit: tally.status,
            by: tally.by,
            agent: call.agent.clone(),
            lines: tally.read.lines as u64,
            who: (!call.agent_id.is_empty()).then(|| call.agent_id.clone()),
        };
        let _ = fs::write(tally.job_dir.join("meta"), meta.to_string());
    }

    /// The digest, then the trailer and what follows it: the same shape every time, on stderr,
    /// where a pipe on the caller's side cannot cut it off.
    pub fn report(&self) {
        let Serving {
            call,
            sink,
            settings,
            machine,
            ..
        } = self.at;
        let tally = &self.tally;
        let host = &machine.host;
        let lines = tally.read.lines;
        if !call.request.stream {
            sink.out(
                &Digest {
                    path: tally.log,
                    lines,
                    head: settings.digest_head,
                    tail: settings.digest_tail,
                    host,
                    job: tally.job,
                }
                .text(),
            );
        }
        let built = tally.read.built;
        sink.record(Record::Trailer(Trailer {
            job: tally.job.clone(),
            mode: call.mode(),
            label: call.label().clone(),
            queued: tally.waited,
            ran: tally.ran,
            exit: tally.status,
            by: tally.by,
            built,
            no_tests: tally.read.tests == Some(0),
        }));
        let mut after = String::new();
        if !call.request.watch.hold {
            after.push_str(&format!(
                "  log {host}:{}  ({lines} lines)  dibs --on {host} --out {}\n",
                tally.log.display(),
                tally.job
            ));
        }
        after.push_str(&tally.hosted.lines(host));
        if built == Some(Built::Nothing) {
            after.push_str(
                "  built nothing: cargo compiled 0 crates, so a measurement after this measures the previous binary.\n",
            );
        }
        if tally.status != 0
            && !call.request.watch.hold
            && let Some(earlier) = (Repeat {
                jobs: &machine.jobs(),
                this: tally.job_dir,
                window: settings.repeat_window,
            })
            .earlier()
        {
            after.push_str(&format!(
                "  this exact command already failed here: job {earlier}. Unchanged, it failed the same way.\n"
            ));
        }
        sink.say(&after);
    }
}
