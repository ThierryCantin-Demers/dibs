use crate::machine::Stream;

/// Where a call's output goes, what this side says about it included.
pub enum Output<'a> {
    /// This process's own streams.
    Inherit,
    /// Read here a line at a time.
    Lines(&'a mut dyn FnMut(Stream, &[u8])),
}

impl Output<'_> {
    /// Says something about the call where its stderr goes.
    pub fn say(&mut self, text: &str) {
        match self {
            Output::Inherit => eprint!("{text}"),
            Output::Lines(on_line) => {
                for line in text.split_inclusive('\n') {
                    on_line(Stream::Err, line.as_bytes());
                }
            }
        }
    }

    /// Whether the machine's output lands on a terminal, which it may colour.
    pub fn tty(&self) -> bool {
        // SAFETY: isatty only reads the descriptor's state.
        matches!(self, Output::Inherit) && unsafe { libc::isatty(1) } == 1
    }
}
