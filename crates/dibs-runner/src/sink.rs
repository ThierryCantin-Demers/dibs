use dibs_format::{
    Exit,
    wire::{Admission, CheckInReport, Frame, Record},
};
use std::{
    fs::File,
    io::{self, Write as _},
    os::fd::AsRawFd as _,
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
    /// Stdout is the report a follower reads, which a write never blocks on.
    handed: Arc<AtomicBool>,
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
            handed: Arc::new(AtomicBool::new(false)),
            out: Arc::new(Mutex::new(io::stdout())),
        }
    }

    pub fn plain() -> Sink {
        Sink {
            plain: Arc::new(AtomicBool::new(true)),
            handed: Arc::new(AtomicBool::new(false)),
            out: Arc::new(Mutex::new(io::stdout())),
        }
    }

    /// A transfer's last frame: from here on stdout is rsync's, and the call's exit its own.
    pub fn transferring(&self) {
        self.record(Record::Transferring);
        self.plain.store(true, Ordering::SeqCst);
    }

    /// Frames to a client, which a check-in can hand a job over from.
    pub fn framed(&self) -> bool {
        self.kind() == Kind::Frames
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
            (Kind::Plain, Record::Trailer(trailer))
            | (Kind::Plain, Record::Admitted(Admission::Ended(trailer))) => {
                self.say(&format!("{}\n", trailer.to_line()))
            }
            (
                Kind::Plain,
                Record::Holding(_)
                | Record::Transferring
                | Record::Prepared(_)
                | Record::Stepped(_)
                | Record::CheckedIn(_)
                | Record::Admitted(
                    Admission::Running { .. } | Admission::Lost { .. } | Admission::Never,
                ),
            ) => {}
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
        let out = match self.handed.load(Ordering::SeqCst) {
            true => Ok(self.out.lock().unwrap_or_else(|e| e.into_inner())),
            false => self.out.try_lock().map_err(|_| ()),
        };
        let Ok(mut out) = out else {
            return;
        };
        // SAFETY: fcntl sets a flag on this process's stdout, which it is about to leave.
        unsafe {
            let flags = libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        // Flushed: stdout is line-buffered, and `_exit` would drop the code after the newline.
        let _ = out
            .write_all(&Frame::Exit(code).to_bytes())
            .and_then(|()| out.flush());
    }

    /// The caller's last words and exit 80; what the call says from here on goes to `report`,
    /// for whoever follows the job, and the caller's streams are let go so its connection can end.
    /// Nothing is let go when the caller cannot be told.
    pub fn hand_over(&self, checked: CheckInReport, text: &str, report: &File) -> io::Result<()> {
        let null = File::options().read(true).write(true).open("/dev/null")?;
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        for frame in [
            Frame::Record(Record::CheckedIn(checked)),
            Frame::Err(text.as_bytes().to_vec()),
            Frame::Exit(Exit::CheckedIn.as_status()),
        ] {
            out.write_all(&frame.to_bytes())?;
        }
        out.flush()?;
        for (from, to) in [
            (report.as_raw_fd(), libc::STDOUT_FILENO),
            (null.as_raw_fd(), libc::STDIN_FILENO),
            (null.as_raw_fd(), libc::STDERR_FILENO),
        ] {
            // SAFETY: dup2 only replaces a descriptor of this process with another it holds.
            if unsafe { libc::dup2(from, to) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        self.handed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn frame(&self, frame: Frame) -> bool {
        self.write(&frame.to_bytes())
    }

    /// Whether the bytes reached whoever reads stdout.
    fn write(&self, bytes: &[u8]) -> bool {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        out.write_all(bytes).and_then(|()| out.flush()).is_ok()
    }
}
