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
    /// A header that is not a word, a space and a length that fits, or no header line within its
    /// longest.
    Header(String),
    /// A header whose word names no frame.
    Kind(String),
    /// A payload that does not read as its kind's.
    Payload { kind: &'static str, why: String },
}

/// A header is a short word and a length, so a longer line is not one.
const HEADER_MAX_BYTES: usize = 32;

impl Frame {
    /// The frame as it crosses the stream: its header, then its payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        let payload = match self {
            Frame::Request(request) => serde_json::to_vec(request).expect("a frame serializes"),
            Frame::Record(record) => serde_json::to_vec(record).expect("a frame serializes"),
            Frame::Beat => Vec::new(),
            Frame::Release(status) | Frame::Exit(status) => status.to_string().into_bytes(),
            Frame::Out(bytes) | Frame::Err(bytes) => bytes.clone(),
        };
        let mut frame = format!("{} {}\n", self.kind(), payload.len()).into_bytes();
        frame.extend_from_slice(&payload);
        frame
    }
}

impl Frame {
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

    fn decode(kind: &str, payload: &[u8]) -> Result<Frame, FrameError> {
        let json = |kind| {
            move |e: serde_json::Error| FrameError::Payload {
                kind,
                why: e.to_string(),
            }
        };
        let status = |kind| {
            let text = String::from_utf8_lossy(payload);
            text.parse().map_err(|_| FrameError::Payload {
                kind,
                why: format!("{text:?} is not a status"),
            })
        };
        match kind {
            "request" => serde_json::from_slice(payload)
                .map(|request| Frame::Request(Box::new(request)))
                .map_err(json("request")),
            "beat" => Ok(Frame::Beat),
            "release" => status("release").map(Frame::Release),
            "out" => Ok(Frame::Out(payload.to_vec())),
            "err" => Ok(Frame::Err(payload.to_vec())),
            "record" => serde_json::from_slice(payload)
                .map(Frame::Record)
                .map_err(json("record")),
            "exit" => status("exit").map(Frame::Exit),
            other => Err(FrameError::Kind(other.to_string())),
        }
    }
}

/// What a stream has carried so far, read back out as frames.
///
/// # Guarantees
///
/// - Every frame [`Frame::to_bytes`] writes reads back equal, however its bytes are split
///   between feeds.
/// - A reader that feeds no more than [`FrameBuffer::wanted`] bytes at a time never takes a byte
///   past the frame, which a transfer's raw stream follows.
#[derive(Debug, Default)]
pub struct FrameBuffer {
    /// The bytes fed and not yet read as a frame, fed at the end and taken from the front.
    carried: Vec<u8>,
}

impl FrameBuffer {
    /// Carries `bytes` after those fed before.
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        self.carried.extend_from_slice(bytes);
    }

    /// The next whole frame, taken out of what was fed, or None until more of it has been fed.
    ///
    /// # Errors
    ///
    /// [`FrameError`]: a frame whose header does not read is left where it is, and one whose kind
    /// or payload does not read is taken out.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        take_frame(&mut self.carried)
    }

    /// How many more bytes the next frame needs: one at a time until its header reads, then the
    /// rest of it exactly.
    ///
    /// # Assumptions
    ///
    /// - [`FrameBuffer::next_frame`] returned None since the last feed.
    pub fn wanted(&self) -> usize {
        Header::wanted_by(&self.carried)
    }
}

/// The frame `carried` starts with, drained from it once whole; a header that does not read
/// leaves `carried` as it was.
fn take_frame(carried: &mut Vec<u8>) -> Result<Option<Frame>, FrameError> {
    let Some(header) = Header::at_front(carried)? else {
        return Ok(None);
    };
    let Some(payload) = carried.get(header.line_bytes..header.frame_bytes) else {
        return Ok(None);
    };
    let frame = Frame::decode(&header.kind, payload);
    carried.drain(..header.frame_bytes);
    frame.map(Some)
}

/// The header line at the front of a stream's carried bytes.
struct Header {
    pub kind: String,
    /// The header line's length, its newline counted.
    pub line_bytes: usize,
    /// The whole frame's length, header and payload.
    pub frame_bytes: usize,
}

impl Header {
    /// The header `carried` starts with, or None while it has no newline within a header's
    /// longest.
    fn at_front(carried: &[u8]) -> Result<Option<Header>, FrameError> {
        let window = &carried[..carried.len().min(HEADER_MAX_BYTES)];
        let Some(end) = window.iter().position(|b| *b == b'\n') else {
            return match carried.len() < HEADER_MAX_BYTES {
                true => Ok(None),
                false => Err(FrameError::Header(
                    String::from_utf8_lossy(window).into_owned(),
                )),
            };
        };
        let line = String::from_utf8_lossy(&window[..end]).into_owned();
        let refused = || FrameError::Header(line.clone());
        let Some((kind, length)) = line.split_once(' ') else {
            return Err(refused());
        };
        let line_bytes = end + 1;
        let Some(frame_bytes) = length
            .parse::<usize>()
            .ok()
            .and_then(|length| line_bytes.checked_add(length))
        else {
            return Err(refused());
        };
        Ok(Some(Header {
            kind: kind.to_string(),
            line_bytes,
            frame_bytes,
        }))
    }

    /// How many more bytes the frame `carried` starts with needs: one while its header does not
    /// read, and at least one.
    fn wanted_by(carried: &[u8]) -> usize {
        match Header::at_front(carried) {
            Ok(Some(header)) => header.frame_bytes.saturating_sub(carried.len()).max(1),
            Ok(None) | Err(_) => 1,
        }
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
        ExitSource, JobId, Label, Mode,
        wire::{Trailer, request::tests::request},
    };

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
                queued_secs: 1,
                ran_secs: 2,
                exit: 0,
                by: ExitSource::Command,
                built: None,
                test_count: None,
            })),
            Frame::Exit(124),
        ]
    }

    fn refusal(bytes: &[u8]) -> FrameError {
        let mut buffer = FrameBuffer::default();
        buffer.feed_bytes(bytes);
        buffer.next_frame().unwrap_err()
    }

    #[test]
    fn every_kind_fed_a_byte_at_a_time_reads_back_as_it_was_sent() {
        let mut buffer = FrameBuffer::default();
        let mut read = Vec::new();
        for byte in every_kind().iter().flat_map(Frame::to_bytes) {
            buffer.feed_bytes(&[byte]);
            while let Some(frame) = buffer.next_frame().unwrap() {
                read.push(frame);
            }
        }
        assert_eq!(read, every_kind());
        assert_eq!(buffer.next_frame(), Ok(None));
    }

    #[test]
    fn a_reader_taking_what_is_wanted_leaves_what_follows_the_frame() {
        let mut stream = Frame::Request(Box::new(request())).to_bytes();
        stream.extend_from_slice(b"rsync's own bytes");
        let mut buffer = FrameBuffer::default();
        let mut at = 0;
        let mut frame = None;
        while frame.is_none() {
            let end = (at + buffer.wanted()).min(stream.len());
            buffer.feed_bytes(&stream[at..end]);
            at = end;
            frame = buffer.next_frame().unwrap();
        }
        assert_eq!(frame, Some(Frame::Request(Box::new(request()))));
        assert_eq!(&stream[at..], b"rsync's own bytes");
    }

    #[test]
    fn one_byte_is_wanted_while_the_length_does_not_read() {
        let mut buffer = FrameBuffer::default();
        buffer.feed_bytes(b"out many\n");
        assert_eq!(buffer.wanted(), 1);
    }

    #[test]
    fn the_header_is_a_word_and_a_length() {
        assert_eq!(Frame::Exit(0).to_bytes(), b"exit 1\n0");
        assert_eq!(Frame::Beat.to_bytes(), b"beat 0\n");
    }

    #[test]
    fn an_unknown_kind_is_refused_as_a_kind() {
        assert_eq!(
            refusal(b"nonsense 0\n"),
            FrameError::Kind("nonsense".into())
        );
    }

    #[test]
    fn a_length_that_is_not_a_number_is_refused_as_a_header() {
        assert_eq!(
            refusal(b"out many\n"),
            FrameError::Header("out many".into())
        );
    }

    #[test]
    fn no_newline_within_a_header_s_longest_is_refused_as_a_header() {
        assert_eq!(
            refusal(&[b'x'; HEADER_MAX_BYTES]),
            FrameError::Header("x".repeat(HEADER_MAX_BYTES))
        );
    }

    #[test]
    fn a_header_with_no_space_is_refused_as_a_header() {
        assert_eq!(refusal(b"out\n"), FrameError::Header("out".into()));
    }

    #[test]
    fn a_length_past_the_largest_is_refused_as_a_header() {
        assert_eq!(
            refusal(b"out 18446744073709551615\n"),
            FrameError::Header("out 18446744073709551615".into())
        );
    }

    #[test]
    fn a_status_that_is_not_a_number_is_refused_as_a_payload() {
        assert_eq!(
            refusal(b"exit 2\nno"),
            FrameError::Payload {
                kind: "exit",
                why: "\"no\" is not a status".into()
            }
        );
    }

    #[test]
    fn a_payload_that_is_not_its_kind_s_json_is_refused_as_a_payload() {
        assert!(matches!(
            refusal(b"request 2\n{}"),
            FrameError::Payload {
                kind: "request",
                ..
            }
        ));
    }
}
