use crate::{
    job::HoldFifo,
    platform::{Host, Platform as _},
    stop::Stopper,
};
use dibs_format::wire::{Frame, FrameError, Request, Unframer};
use std::{
    fmt,
    fs::File,
    io::{self, Read as _},
    os::fd::{AsFd as _, AsRawFd as _},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

/// The caller's side of the stream: the request, then what says it is still there.
pub struct Channel {
    input: File,
    unframer: Unframer,
}

/// How the runner hears whether its caller is still there.
pub enum Caller {
    /// Frames on stdin: beats, a release, and its end.
    Channel(Channel),
    /// Whoever reads stdout closing it: a transfer's and a build's, whose stdin carries rsync's
    /// stream or a tree.
    Stdout,
}

impl Caller {
    pub fn channel(self) -> Option<Channel> {
        match self {
            Caller::Channel(channel) => Some(channel),
            Caller::Stdout => None,
        }
    }
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
    /// A copy of stdin, so dropping the channel leaves stdin to a transfer's job, which reads
    /// what follows the request.
    pub fn stdin() -> io::Result<Channel> {
        Ok(Channel {
            input: File::from(io::stdin().as_fd().try_clone_to_owned()?),
            unframer: Unframer::default(),
        })
    }

    /// The first frame, which is the call.
    pub fn request(&mut self) -> Result<Request, ChannelError> {
        match self.hear(None)? {
            Heard::Frame(Frame::Request(request)) => Ok(*request),
            _ => Err(ChannelError::NoRequest),
        }
    }

    /// The next frame, the end of the stream, or silence for `within`.
    fn hear(&mut self, within: Option<Duration>) -> Result<Heard, ChannelError> {
        let mut chunk = [0u8; 4096];
        let mut frame = self.unframer.next_frame().map_err(ChannelError::Frame)?;
        while frame.is_none() {
            if within.is_some_and(|span| !self.readable(span)) {
                return Ok(Heard::Silent);
            }
            let wanted = self.unframer.wanted().min(chunk.len());
            match self.input.read(&mut chunk[..wanted]) {
                Ok(0) => return Ok(Heard::Ended),
                Ok(n) => self.unframer.feed(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ChannelError::Read(e)),
            }
            frame = self.unframer.next_frame().map_err(ChannelError::Frame)?;
        }
        Ok(frame.map_or(Heard::Ended, Heard::Frame))
    }

    /// Whether something arrives within the span.
    fn readable(&self, span: Duration) -> bool {
        let mut fd = libc::pollfd {
            fd: self.input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = libc::c_int::try_from(span.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: one pollfd, owned here, for the call's length.
        unsafe { libc::poll(&mut fd, 1, millis) > 0 }
    }

    /// A transfer's stdin is rsync's, which reads none of it while it prepares a tree, and a
    /// build's is its tree, so their caller is watched another way.
    pub fn hangup(stopper: Arc<Stopper>) {
        thread::spawn(move || {
            Host::await_caller_gone();
            stopper.caller_gone(&Gone::Ended.to_string());
        });
    }

    /// Watches the stream on a thread of its own: its end, or silence past the lease, is the
    /// caller gone. A caller alive says something at least once a lease, and one that sleeps
    /// closes nothing. A hold ends with a release, which is its caller's command ending, not the
    /// caller going.
    /// Listens until `until`; false once the caller has gone, its stream ended or silent for
    /// longer than the lease since it was last `heard`.
    pub fn attend(&mut self, until: Instant, lease: u64, heard: &mut Instant) -> bool {
        let lease = Duration::from_secs(lease);
        let mut present = true;
        while present && Instant::now() < until {
            present = match self.hear(Some(until.saturating_duration_since(Instant::now()))) {
                Ok(Heard::Frame(_)) => {
                    *heard = Instant::now();
                    true
                }
                Ok(Heard::Silent) => lease.is_zero() || heard.elapsed() <= lease,
                Ok(Heard::Ended) | Err(_) => false,
            };
        }
        present
    }

    pub fn watch(mut self, lease: u64, stopper: Arc<Stopper>, held: Option<HoldFifo>) {
        let within = (lease > 0).then(|| Duration::from_secs(lease));
        thread::spawn(move || {
            let end = std::iter::repeat_with(|| self.hear(within))
                .find_map(|heard| match (heard, &held) {
                    (Ok(Heard::Frame(Frame::Release(status))), Some(held)) => {
                        held.release(status);
                        Some(End::Released)
                    }
                    (Ok(Heard::Frame(_)), _) => None,
                    (Ok(Heard::Silent), _) => Some(End::Gone(Gone::Silent(lease))),
                    _ => Some(End::Gone(Gone::Ended)),
                })
                .unwrap_or(End::Gone(Gone::Ended));
            if let End::Gone(gone) = end {
                stopper.caller_gone(&gone.to_string());
            }
        });
    }
}

/// How watching a caller ended.
enum End {
    Gone(Gone),
    Released,
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
