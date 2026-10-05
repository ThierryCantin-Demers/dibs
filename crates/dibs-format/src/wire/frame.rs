use crate::wire::{Record, Request};
use std::fmt;

/// One message between a client and its runner: `<kind> <length>\n` and then the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Request(Box<Request>),
    Beat,
    /// A held command's exit.
    Release(i32),
    Out(Vec<u8>),
    Err(Vec<u8>),
    Record(Record),
    Exit(i32),
}

/// Why bytes did not read as frames. Nothing after one can be trusted, so a reader stops there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    Header(String),
    Kind(String),
    Payload { kind: &'static str, why: String },
}

/// A header is a short word and a length, so a longer line is not one.
const HEADER_MAX: usize = 32;

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let payload = match self {
            Frame::Request(request) => serde_json::to_vec(request).unwrap_or_default(),
            Frame::Record(record) => serde_json::to_vec(record).unwrap_or_default(),
            Frame::Beat => Vec::new(),
            Frame::Release(status) | Frame::Exit(status) => status.to_string().into_bytes(),
            Frame::Out(bytes) | Frame::Err(bytes) => bytes.clone(),
        };
        let mut frame = format!("{} {}\n", self.kind(), payload.len()).into_bytes();
        frame.extend_from_slice(&payload);
        frame
    }

    fn kind(&self) -> &'static str {
        match self {
            Frame::Request(_) => "request",
            Frame::Beat => "beat",
            Frame::Release(_) => "release",
            Frame::Out(_) => "out",
            Frame::Err(_) => "err",
            Frame::Record(_) => "record",
            Frame::Exit(_) => "exit",
        }
    }

    fn decode(kind: &str, payload: Vec<u8>) -> Result<Frame, FrameError> {
        let json = |kind| {
            move |e: serde_json::Error| FrameError::Payload {
                kind,
                why: e.to_string(),
            }
        };
        let status = |kind| {
            let text = String::from_utf8_lossy(&payload);
            text.parse().map_err(|_| FrameError::Payload {
                kind,
                why: format!("{text:?} is not a status"),
            })
        };
        match kind {
            "request" => serde_json::from_slice(&payload)
                .map(|request| Frame::Request(Box::new(request)))
                .map_err(json("request")),
            "beat" => Ok(Frame::Beat),
            "release" => status("release").map(Frame::Release),
            "out" => Ok(Frame::Out(payload)),
            "err" => Ok(Frame::Err(payload)),
            "record" => serde_json::from_slice(&payload)
                .map(Frame::Record)
                .map_err(json("record")),
            "exit" => status("exit").map(Frame::Exit),
            other => Err(FrameError::Kind(other.to_string())),
        }
    }
}

/// Frames read back out of what a stream has carried so far.
#[derive(Debug, Default)]
pub struct Unframer {
    carried: Vec<u8>,
}

impl Unframer {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.carried.extend_from_slice(bytes);
    }

    /// The next whole frame, or None until more of it has been fed.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        let window = &self.carried[..self.carried.len().min(HEADER_MAX)];
        let Some(end) = window.iter().position(|b| *b == b'\n') else {
            return match self.carried.len() < HEADER_MAX {
                true => Ok(None),
                false => Err(FrameError::Header(
                    String::from_utf8_lossy(window).into_owned(),
                )),
            };
        };
        let header = String::from_utf8_lossy(&self.carried[..end]).into_owned();
        let Some((kind, length)) = header.split_once(' ') else {
            return Err(FrameError::Header(header));
        };
        let length: usize = length
            .parse()
            .map_err(|_| FrameError::Header(header.clone()))?;
        let start = end + 1;
        if self.carried.len() < start + length {
            return Ok(None);
        }
        let payload = self.carried[start..start + length].to_vec();
        self.carried.drain(..start + length);
        Frame::decode(kind, payload).map(Some)
    }

    /// How many more bytes the next frame needs: one at a time until its header is in, then the
    /// rest of it exactly. A reader that takes no more than this never takes what follows the
    /// frame, which a transfer's raw stream does.
    pub fn wanted(&self) -> usize {
        let window = &self.carried[..self.carried.len().min(HEADER_MAX)];
        let Some(end) = window.iter().position(|b| *b == b'\n') else {
            return 1;
        };
        let length = String::from_utf8_lossy(&window[..end])
            .split_once(' ')
            .and_then(|(_, length)| length.parse::<usize>().ok())
            .unwrap_or_default();
        (end + 1 + length).saturating_sub(self.carried.len()).max(1)
    }

    /// Whether part of a frame is still waiting for the rest.
    pub fn is_empty(&self) -> bool {
        self.carried.is_empty()
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Header(header) => write!(f, "{header:?} is not a frame header"),
            FrameError::Kind(kind) => write!(f, "no frame is called {kind:?}"),
            FrameError::Payload { kind, why } => write!(f, "a {kind} frame did not read: {why}"),
        }
    }
}

impl std::error::Error for FrameError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        By, JobId, Label, Mode,
        wire::{MaxFrom, Trailer, Watch},
    };

    fn request() -> Request {
        Request {
            mode: Mode::Shared,
            label: Label::new("build"),
            command: "echo 'a\nb'\t$HOME".into(),
            wait: None,
            max: 7200,
            max_from: MaxFrom::Default,
            verbose: false,
            json: false,
            stream: false,
            tty: false,
            card: None,
            fingerprint: None,
            agent: "session".into(),
            agent_id: "id".into(),
            batch: None,
            watch: Watch {
                off: false,
                hold: false,
                lease: 120,
            },
            ports: vec!["api".into()],
            services: Vec::new(),
            ready_within: 300,
            new_series: false,
            tree: None,
        }
    }

    fn every_kind() -> Vec<Frame> {
        vec![
            Frame::Request(Box::new(request())),
            Frame::Beat,
            Frame::Release(3),
            Frame::Out(b"bytes\0with\nanything\n".to_vec()),
            Frame::Err(Vec::new()),
            Frame::Record(Record::Trailer(Trailer {
                job: JobId::new("20261001-120000-1"),
                mode: Mode::Bench,
                label: Label::new("b"),
                queued: 1,
                ran: 2,
                exit: 0,
                by: By::Command,
                built: None,
            })),
            Frame::Exit(124),
        ]
    }

    #[test]
    fn every_kind_reads_back_as_it_was_sent() {
        let frames = every_kind();
        let mut unframer = Unframer::default();
        unframer.feed(&frames.iter().flat_map(Frame::encode).collect::<Vec<_>>());
        for frame in frames {
            assert_eq!(unframer.next_frame(), Ok(Some(frame)));
        }
        assert_eq!(unframer.next_frame(), Ok(None));
        assert!(unframer.is_empty());
    }

    #[test]
    fn a_frame_fed_a_byte_at_a_time_reads_once_it_is_whole() {
        let mut unframer = Unframer::default();
        let bytes: Vec<u8> = every_kind().iter().flat_map(Frame::encode).collect();
        let mut read = Vec::new();
        for byte in bytes {
            unframer.feed(&[byte]);
            while let Some(frame) = unframer.next_frame().unwrap() {
                read.push(frame);
            }
        }
        assert_eq!(read, every_kind());
    }

    #[test]
    fn a_reader_taking_what_is_wanted_leaves_what_follows_the_frame() {
        let mut stream = Frame::Request(Box::new(request())).encode();
        stream.extend_from_slice(b"rsync's own bytes");
        let mut unframer = Unframer::default();
        let mut at = 0;
        let mut frame = None;
        while frame.is_none() {
            let end = (at + unframer.wanted()).min(stream.len());
            unframer.feed(&stream[at..end]);
            at = end;
            frame = unframer.next_frame().unwrap();
        }
        assert_eq!(frame, Some(Frame::Request(Box::new(request()))));
        assert_eq!(&stream[at..], b"rsync's own bytes");
    }

    #[test]
    fn the_header_is_a_word_and_a_length() {
        assert_eq!(Frame::Exit(0).encode(), b"exit 1\n0");
        assert_eq!(Frame::Beat.encode(), b"beat 0\n");
    }

    #[test]
    fn what_is_not_a_frame_is_refused() {
        let refused = |bytes: &[u8]| {
            let mut unframer = Unframer::default();
            unframer.feed(bytes);
            unframer.next_frame().is_err()
        };
        assert!(refused(b"nonsense 0\n"));
        assert!(refused(b"out many\n"));
        assert!(refused(&[b'x'; HEADER_MAX]));
        assert!(refused(b"exit 2\nno"));
        assert!(refused(b"request 2\n{}"));
    }
}
