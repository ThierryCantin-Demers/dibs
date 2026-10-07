use crate::Span;
use std::time::{SystemTime, UNIX_EPOCH};

/// A moment on a clock some seconds east of UTC, as records, ids and messages spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    /// Seconds east of UTC.
    offset: i64,
}

impl Moment {
    /// Now, in this computer's zone.
    pub fn now() -> Moment {
        Moment::at(Moment::epoch_now())
    }

    /// Seconds since the epoch.
    pub fn epoch_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    }

    /// `epoch` in this computer's zone, with the offset it had then.
    pub fn at(epoch: u64) -> Moment {
        let time = epoch as libc::time_t;
        // SAFETY: tm is plain data, and localtime_r fills it from a valid time.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&time, &mut tm) };
        Moment::in_zone(epoch, tm.tm_gmtoff as i64)
    }

    /// `epoch` on a clock `offset` seconds east of UTC, by the days-to-civil conversion.
    pub fn in_zone(epoch: u64, offset: i64) -> Moment {
        let day = Span::DAY.0 as i64;
        let t = epoch as i64 + offset;
        let (days, secs) = (t.div_euclid(day), t.rem_euclid(day));
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        Moment {
            year: yoe + era * 400 + i64::from(month <= 2),
            month,
            day: doy - (153 * mp + 2) / 5 + 1,
            hour: secs / 3600,
            minute: secs / 60 % 60,
            second: secs % 60,
            offset,
        }
    }

    /// Seconds east of UTC.
    pub fn offset(&self) -> i64 {
        self.offset
    }

    /// As `date -Is` prints it: `2026-10-01T12:00:00+02:00`.
    pub fn iso(&self) -> String {
        let sign = if self.offset < 0 { '-' } else { '+' };
        let offset = self.offset.unsigned_abs();
        format!(
            "{}T{}{sign}{:02}:{:02}",
            self.day(),
            self.clock(),
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

    /// `2026-10-01 12:00`, as a record's time is listed.
    pub fn minute(&self) -> String {
        format!("{} {:02}:{:02}", self.day(), self.hour, self.minute)
    }

    /// `20261001120000`, the start of a job id.
    pub fn compact(&self) -> String {
        format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `20261001-120000`, the start of a batch id.
    pub fn dashed(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moment_is_civil_time_on_its_clock() {
        assert_eq!(Moment::in_zone(0, 0).minute(), "1970-01-01 00:00");
        assert_eq!(Moment::in_zone(1789745748, 0).minute(), "2026-09-18 15:35");
        assert_eq!(Moment::in_zone(951782400, 0).minute(), "2000-02-29 00:00");
        assert_eq!(Moment::in_zone(0, -3600).minute(), "1969-12-31 23:00");
    }

    #[test]
    fn a_moment_spells_itself_as_date_does() {
        let moment = Moment::in_zone(1789745748, 2 * 3600 + 30 * 60);
        assert_eq!(moment.iso(), "2026-09-18T18:05:48+02:30");
        assert_eq!(moment.compact(), "20260918180548");
        assert_eq!(moment.dashed(), "20260918-180548");
        assert_eq!(
            Moment::in_zone(0, -5 * 3600).iso(),
            "1969-12-31T19:00:00-05:00"
        );
    }

    #[test]
    fn now_here_is_now_on_its_own_offset() {
        let epoch = Moment::epoch_now();
        let here = Moment::at(epoch);
        assert_eq!(here, Moment::in_zone(epoch, here.offset()));
    }
}
