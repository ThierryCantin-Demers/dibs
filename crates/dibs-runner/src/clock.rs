pub use dibs_format::{Moment, Span};
use std::{
    thread,
    time::{Duration, Instant},
};

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
