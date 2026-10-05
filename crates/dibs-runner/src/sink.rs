use dibs_format::wire::{Frame, Record};
use std::{
    io::{self, Write as _},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// Where a call's output goes: frames to a client, one at a time whichever thread writes one,
/// or plain text to whoever ran `build`, or to rsync once a transfer is under way.
#[derive(Clone)]
pub struct Sink {
    plain: Arc<AtomicBool>,
    out: Arc<Mutex<io::Stdout>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Frames,
    Plain,
}

impl Sink {
    pub fn frames() -> Sink {
        Sink {
            plain: Arc::new(AtomicBool::new(false)),
            out: Arc::new(Mutex::new(io::stdout())),
        }
    }

    pub fn plain() -> Sink {
        Sink {
            plain: Arc::new(AtomicBool::new(true)),
            out: Arc::new(Mutex::new(io::stdout())),
        }
    }

    /// A transfer's last frame: from here on stdout is rsync's, and the call's exit its own.
    pub fn transferring(&self) {
        self.record(Record::Transferring);
        self.plain.store(true, Ordering::SeqCst);
    }

    fn kind(&self) -> Kind {
        match self.plain.load(Ordering::SeqCst) {
            true => Kind::Plain,
            false => Kind::Frames,
        }
    }

    /// Bytes for the caller's stdout; false once nobody reads them.
    pub fn out(&self, bytes: &[u8]) -> bool {
        match self.kind() {
            Kind::Frames => self.frame(Frame::Out(bytes.to_vec())),
            Kind::Plain => self.write(bytes),
        }
    }

    /// Text for the caller's stderr.
    pub fn say(&self, text: &str) {
        if !text.is_empty() {
            self.err(text.as_bytes());
        }
    }

    pub fn err(&self, bytes: &[u8]) {
        match self.kind() {
            Kind::Frames => {
                self.frame(Frame::Err(bytes.to_vec()));
            }
            Kind::Plain => {
                let _ = io::stderr().write_all(bytes);
            }
        }
    }

    pub fn record(&self, record: Record) {
        match (self.kind(), record) {
            (Kind::Frames, record) => {
                self.frame(Frame::Record(record));
            }
            (Kind::Plain, Record::Trailer(trailer)) => self.say(&format!("{trailer}\n")),
            (Kind::Plain, Record::Holding(_) | Record::Transferring | Record::Prepared(_)) => {}
        }
    }

    /// The call's exit, the last frame it sends.
    pub fn exit(&self, code: i32) {
        if self.kind() == Kind::Frames {
            self.frame(Frame::Exit(code));
        }
    }

    /// The exit of a call being stopped, which ends however its caller is: never behind a relay
    /// that holds stdout, nor behind a reader that stopped reading.
    pub fn exit_without_waiting(&self, code: i32) {
        if self.kind() != Kind::Frames {
            return;
        }
        let Ok(mut out) = self.out.try_lock() else {
            return;
        };
        // SAFETY: fcntl sets a flag on this process's stdout, which it is about to leave.
        unsafe {
            let flags = libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        let _ = out.write_all(&Frame::Exit(code).encode());
    }

    fn frame(&self, frame: Frame) -> bool {
        self.write(&frame.encode())
    }

    /// Whether the bytes reached whoever reads stdout.
    fn write(&self, bytes: &[u8]) -> bool {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        out.write_all(bytes).and_then(|()| out.flush()).is_ok()
    }
}
