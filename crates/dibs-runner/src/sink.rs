use dibs_format::wire::{Frame, Record};
use std::{
    io::{self, Write as _},
    sync::{Arc, Mutex},
};

/// The caller's side of the call's output: frames on stdout, one at a time whichever thread
/// writes it.
#[derive(Clone)]
pub struct Sink {
    out: Arc<Mutex<io::Stdout>>,
}

impl Sink {
    pub fn frames() -> Sink {
        Sink {
            out: Arc::new(Mutex::new(io::stdout())),
        }
    }

    /// Bytes for the caller's stdout.
    pub fn out(&self, bytes: &[u8]) {
        self.frame(Frame::Out(bytes.to_vec()));
    }

    /// Text for the caller's stderr.
    pub fn say(&self, text: &str) {
        if !text.is_empty() {
            self.err(text.as_bytes());
        }
    }

    pub fn err(&self, bytes: &[u8]) {
        self.frame(Frame::Err(bytes.to_vec()));
    }

    pub fn record(&self, record: Record) {
        self.frame(Frame::Record(record));
    }

    /// The call's exit, the last frame it sends.
    pub fn exit(&self, code: i32) {
        self.frame(Frame::Exit(code));
    }

    /// A caller that has gone cannot be written to, which changes nothing here.
    fn frame(&self, frame: Frame) {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = out.write_all(&frame.encode()).and_then(|()| out.flush());
    }
}
