use crate::{
    call::machine::{Asked, MachineCall},
    caller::Caller,
    cli::Call,
    machine::{Deadline, Stop, Stoppable, Stream},
};
use dibs_format::{Label, MachineName, Mode, status::Status};
use std::thread;

/// What a status feed hands its reader.
pub enum Fed {
    Status(Box<Status>),
    /// A line of output that is not a status document, and why.
    Unreadable(String),
    /// A line on stderr while the watch runs, such as the runner being built before it starts.
    Said(String),
    /// The watch is over: what it said on stderr, and why it ended.
    Ended {
        said: String,
        why: String,
    },
}

/// One machine's status every few seconds, as `dibs --watch --json` prints it, read in this
/// process on a thread of its own. Dropping it stops the watch.
pub struct StatusFeed {
    _stop: Stop,
}

impl StatusFeed {
    /// `machine` None watches wherever a bare `dibs` would.
    pub fn start(
        machine: Option<MachineName>,
        every: u64,
        mut on: impl FnMut(Fed) + Send + 'static,
    ) -> StatusFeed {
        let Stoppable { deadline, stop } = Deadline::stoppable();
        thread::spawn(move || {
            let call = Call {
                on: machine,
                json: true,
                ..Call::default()
            };
            let mut said = String::new();
            let mut on_line = |stream: Stream, line: &[u8]| {
                let text = String::from_utf8_lossy(line);
                match stream {
                    Stream::Err => {
                        said.push_str(&text);
                        on(Fed::Said(text.trim_end().to_string()));
                    }
                    Stream::Out if text.starts_with('{') => {
                        on(match serde_json::from_str::<Status>(text.trim_end()) {
                            Ok(status) => Fed::Status(Box::new(status)),
                            Err(e) => Fed::Unreadable(format!("unreadable document: {e}")),
                        })
                    }
                    Stream::Out => {}
                }
            };
            let caller = Caller::from_env();
            let read = MachineCall::new(&call, &caller).and_then(|machine| {
                machine.read_until(
                    Asked::plain(Mode::Watch, Label::new(every.to_string())),
                    deadline,
                    &mut on_line,
                )
            });
            let why = match read {
                Ok(_) => "the feed closed".to_string(),
                Err(e) => e.to_string().trim().to_string(),
            };
            on(Fed::Ended {
                said: said.trim().to_string(),
                why,
            });
        });
        StatusFeed { _stop: stop }
    }
}
