use std::fmt;

/// A duration in seconds as every message writes it: `42s`, `3m05s`, `1h02m`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span(pub u64);

impl Span {
    pub const DAY: Span = Span(86_400);
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0;
        match s {
            3600.. => write!(f, "{}h{:02}m", s / 3600, s % 3600 / 60),
            60.. => write!(f, "{}m{:02}s", s / 60, s % 60),
            _ => write!(f, "{s}s"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_reads_as_the_machine_half_wrote_it() {
        assert_eq!(Span(0).to_string(), "0s");
        assert_eq!(Span(59).to_string(), "59s");
        assert_eq!(Span(185).to_string(), "3m05s");
        assert_eq!(Span(3720).to_string(), "1h02m");
    }
}
