use crate::machine::{Listener, Stream};
use dibs_format::wire::Prepared;

/// Where a call's output goes, what this side says about it included.
pub enum Output<'a> {
    /// This process's own streams.
    Inherit,
    /// Read here a line at a time.
    Lines(&'a mut dyn FnMut(Stream, &[u8])),
    /// Read here a line at a time, with the facts the machine tells besides.
    Listening(&'a mut dyn Listener),
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
            Output::Listening(listener) => {
                for line in text.split_inclusive('\n') {
                    listener.line(Stream::Err, line.as_bytes());
                }
            }
        }
    }

    /// The job's tree, laid out, for whoever listens for it.
    pub fn prepared(&mut self, prepared: &Prepared) {
        if let Output::Listening(listener) = self {
            listener.prepared(prepared);
        }
    }

    /// Whether the machine's output lands on a terminal, which it may colour.
    pub fn tty(&self) -> bool {
        // SAFETY: isatty only reads the descriptor's state.
        matches!(self, Output::Inherit) && unsafe { libc::isatty(1) } == 1
    }
}
