//! What every sensor read just before a benchmark's command and just after it, appended to the file
//! `temperatures` beside the history, so heat shows up next to the jobs that ran into it.

use crate::{
    call::Received,
    clock::Moment,
    platform::{Host, Platform as _},
    shared::SharedFile,
};
use dibs_format::JobId;
use std::path::PathBuf;

/// The readings of one benchmark job.
pub struct Thermals<'a> {
    pub path: PathBuf,
    pub call: &'a Received,
    pub job: &'a JobId,
}

impl Thermals<'_> {
    /// One line: when, `start` or `end`, the job, its label and card, then the readings.
    pub fn record(&self, phase: &str) {
        let card = self
            .call
            .request
            .card
            .as_ref()
            .map_or("none".to_string(), |c| c.alias.to_string());
        let line = format!(
            "{}\t{phase}\t{}\t{}\t{card}\t{}",
            Moment::epoch_now(),
            self.job,
            self.call.label(),
            Host::temperatures()
        );
        let _ = SharedFile { path: &self.path }.append(&line);
    }
}
