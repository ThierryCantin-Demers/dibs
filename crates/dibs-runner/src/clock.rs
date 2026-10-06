pub use dibs_format::Span;
use std::{
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// A moment in the machine's own zone, as its records and job ids spell it.
#[derive(Debug, Clone, Copy)]
pub struct Moment {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    /// Seconds east of UTC.
    offset: i64,
}

impl Moment {
    pub fn now() -> Moment {
        Moment::at(Moment::epoch_now())
    }

    pub fn epoch_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    }

    pub fn at(epoch: u64) -> Moment {
        let time = epoch as libc::time_t;
        // SAFETY: tm is plain data, and localtime_r fills it from a valid time.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&time, &mut tm) };
        Moment {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
            offset: tm.tm_gmtoff as i64,
        }
    }

    /// As `date -Is` prints it: `2026-10-01T12:00:00+02:00`.
    pub fn iso(&self) -> String {
        let sign = if self.offset < 0 { '-' } else { '+' };
        let offset = self.offset.unsigned_abs();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
            offset / 3600,
            offset % 3600 / 60
        )
    }

    /// `2026-10-01`, as `date +%F` prints it.
    pub fn day(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `12:00:00`, as `date +%H:%M:%S` prints it.
    pub fn clock(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// `20261001120000`, the start of a job id.
    pub fn compact(&self) -> String {
        format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// When something must have ended, if anything bounds it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Deadline(Option<Instant>);

impl Deadline {
    /// `cap` from now, or nothing for no cap.
    pub fn after(cap: Option<Duration>) -> Deadline {
        Deadline(cap.map(|cap| Instant::now() + cap))
    }

    pub fn passed(self) -> bool {
        self.0.is_some_and(|at| Instant::now() >= at)
    }

    /// What is left of it, None when nothing bounds it.
    pub fn left(self) -> Option<Duration> {
        self.0
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// The sooner of it and `wait` from now.
    pub fn within(self, wait: Duration) -> Duration {
        self.left().map_or(wait, |left| left.min(wait))
    }

    /// Whether `ready` became true before it passed, asking again every `every`.
    pub fn until(self, every: Duration, ready: impl Fn() -> bool) -> bool {
        let mut done = ready();
        while !done && !self.passed() {
            thread::sleep(self.within(every));
            done = ready();
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moment_spells_itself_as_date_does() {
        let moment = Moment::now();
        let iso = moment.iso();
        assert_eq!(iso.len(), 25, "{iso}");
        assert_eq!(&iso[..4], &moment.compact()[..4]);
    }
}
