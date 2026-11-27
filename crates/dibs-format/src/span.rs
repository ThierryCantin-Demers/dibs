use std::fmt;

/// A duration in seconds as every message writes it: `42s`, `3m05s`, `1h02m`.
///
/// # Guarantees
///
/// - It is written in seconds under a minute, in minutes and seconds under an hour, and in hours
///   and minutes past one, the seconds left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span(u64);

impl Span {
    pub const DAY: Span = Span(86_400);

    pub const fn from_secs(secs: u64) -> Span {
        Span(secs)
    }

    pub const fn as_secs(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_span(f, self.0)
    }
}

fn write_span(f: &mut fmt::Formatter<'_>, secs: u64) -> fmt::Result {
    match secs {
        3600.. => write!(f, "{}h{:02}m", secs / 3600, secs % 3600 / 60),
        60.. => write!(f, "{}m{:02}s", secs / 60, secs % 60),
        _ => write!(f, "{secs}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_reads_as_the_machine_half_wrote_it() {
        assert_eq!(Span::from_secs(0).to_string(), "0s");
        assert_eq!(Span::from_secs(59).to_string(), "59s");
        assert_eq!(Span::from_secs(185).to_string(), "3m05s");
        assert_eq!(Span::from_secs(3720).to_string(), "1h02m");
    }
}
