use crate::stop::Stopper;
use dibs_format::wire::{Frame, FrameError, Request, Unframer};
use std::{
    fmt,
    fs::File,
    io::{self, Read as _},
    os::fd::{AsRawFd as _, FromRawFd as _},
    sync::Arc,
    thread,
};

/// The caller's side of the stream: the request, then what says it is still there.
pub struct Channel {
    input: File,
    unframer: Unframer,
}

/// Why no request could be read.
#[derive(Debug)]
pub enum ChannelError {
    Read(io::Error),
    Frame(FrameError),
    /// The stream ended, or carried something else, before a request.
    NoRequest,
}

/// What the stream said next.
enum Heard {
    Frame(Frame),
    Ended,
    Silent,
}

/// How a caller was found to be gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gone {
    Ended,
    Silent(u64),
}

impl Channel {
    pub fn stdin() -> Channel {
        // SAFETY: nothing else in the runner reads its stdin, which this takes over.
        let input = unsafe { File::from_raw_fd(0) };
        Channel {
            input,
            unframer: Unframer::default(),
        }
    }

    /// The first frame, which is the call.
    pub fn request(&mut self) -> Result<Request, ChannelError> {
        match self.hear(None)? {
            Heard::Frame(Frame::Request(request)) => Ok(*request),
            _ => Err(ChannelError::NoRequest),
        }
    }

    /// The next frame, the end of the stream, or silence for `within` seconds.
    fn hear(&mut self, within: Option<u64>) -> Result<Heard, ChannelError> {
        let mut chunk = [0u8; 4096];
        let mut frame = self.unframer.next_frame().map_err(ChannelError::Frame)?;
        while frame.is_none() {
            if within.is_some_and(|secs| !self.readable(secs)) {
                return Ok(Heard::Silent);
            }
            match self.input.read(&mut chunk) {
                Ok(0) => return Ok(Heard::Ended),
                Ok(n) => self.unframer.feed(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ChannelError::Read(e)),
            }
            frame = self.unframer.next_frame().map_err(ChannelError::Frame)?;
        }
        Ok(frame.map_or(Heard::Ended, Heard::Frame))
    }

    /// Whether something arrives within so many seconds.
    fn readable(&self, secs: u64) -> bool {
        let mut fd = libc::pollfd {
            fd: self.input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = libc::c_int::try_from(secs.saturating_mul(1000)).unwrap_or(libc::c_int::MAX);
        // SAFETY: one pollfd, owned here, for the call's length.
        unsafe { libc::poll(&mut fd, 1, millis) > 0 }
    }

    /// Watches the stream on a thread of its own: its end, or silence past the lease, is the
    /// caller gone. A caller alive says something at least once a lease, and one that sleeps
    /// closes nothing.
    pub fn watch(mut self, lease: u64, stopper: Arc<Stopper>) {
        let within = (lease > 0).then_some(lease);
        thread::spawn(move || {
            let gone = std::iter::repeat_with(|| self.hear(within))
                .find_map(|heard| match heard {
                    Ok(Heard::Frame(_)) => None,
                    Ok(Heard::Silent) => Some(Gone::Silent(lease)),
                    _ => Some(Gone::Ended),
                })
                .unwrap_or(Gone::Ended);
            stopper.caller_gone(&gone.to_string());
        });
    }
}

impl fmt::Display for Gone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Gone::Ended => f.write_str("caller gone"),
            Gone::Silent(lease) => write!(f, "caller silent for {lease}s"),
        }
    }
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelError::Read(e) => write!(f, "the request could not be read: {e}"),
            ChannelError::Frame(e) => write!(f, "the request did not read: {e}"),
            ChannelError::NoRequest => f.write_str("the stream carried no request"),
        }
    }
}
