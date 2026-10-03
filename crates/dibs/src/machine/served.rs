use crate::machine::{
    held::{Held, Holder, Release},
    interrupt::Interrupt,
    lines::Stream,
    payload::{CallValues, Watch},
    provision::{Installed, Provision},
    session::{Liveness, Message, Route, SSH_FAILED, Session, exit_code},
    ssh::{Ssh, parent_death_signal},
};
use dibs_format::{
    Exit,
    wire::{Frame, Picked, Record, Unframer},
};
use std::{
    io::{self, BufRead as _, BufReader, Read, Write as _},
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::{ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

/// What a far shell exits with when the runner for this source is not there.
pub(super) const MISSING: i32 = 125;
/// The word the client's own binary serves the runner under, on this computer.
pub const RUNNER_WORD: &str = "__runner";

/// The runner this binary was built with: the hash of its source, which names the one runner a
/// machine runs for it.
pub struct Runner;

impl Runner {
    pub const HASH: &str = env!("DIBS_RUNNER_HASH");
    /// The tree a machine builds it from, as a gzipped tar.
    pub const SOURCE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runner-source.tar.gz"));

    /// The line the login shell there runs, which fish, bash and dash read alike.
    fn far_line() -> String {
        format!(
            "sh -c 'r=$HOME/.cache/dibs/runner/{}/dibs-runner; [ -x \"$r\" ] || exit {MISSING}; exec \"$r\" serve'",
            Runner::HASH
        )
    }

    /// This binary, which links the runner. Through `/proc` on Linux, so a binary an update has
    /// replaced still starts the runner it was built with.
    fn here() -> Command {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("dibs"));
        let mut command = match cfg!(target_os = "linux") {
            true => {
                let mut command = Command::new("/proc/self/exe");
                command.arg0(&exe);
                command
            }
            false => Command::new(&exe),
        };
        command.args([RUNNER_WORD, "serve"]);
        command
    }
}

/// Where a runner call's output goes.
pub enum Delivery<'a> {
    /// This process's own streams.
    Inherit,
    /// Read here a line at a time.
    Lines(&'a mut dyn FnMut(Stream, &[u8])),
}

/// A call the runner serves, from this side: the request goes in as a frame, and what comes back
/// is the call's output and its exit.
pub struct Served<'a> {
    pub session: &'a Session,
    pub values: &'a CallValues,
    pub live: Liveness,
    /// A hold's: told once the lock is held, with what releases it.
    pub holding: Option<Sender<Held>>,
}

/// What reached this side from the runner.
pub(super) enum Heard {
    Out(Vec<u8>),
    Err(Vec<u8>),
    /// A line the runner or ssh wrote on stderr, outside any frame.
    Line(Vec<u8>),
    Holding(Vec<Picked>),
    Exit(i32),
    Broken(String),
}

impl Served<'_> {
    /// Runs the call and returns its exit. A machine without this runner has it built there
    /// first, through the newest runner it has, and the call is made again.
    pub fn run(&self, delivery: Delivery) -> io::Result<i32> {
        let mut delivery = delivery;
        let deferred = self.holding.is_none().then(Interrupt::defer);
        let answer = self.answer(&mut delivery);
        drop(deferred);
        answer
    }

    /// A hold's side, on a thread of its own: Ctrl-C belongs to the command here, so the runner
    /// is started ignoring it, and so is ssh.
    pub fn holder(session: Session, values: CallValues, live: Liveness) -> Holder {
        let (tell, held) = mpsc::channel();
        let ended = thread::spawn(move || {
            Served {
                session: &session,
                values: &values,
                live,
                holding: Some(tell),
            }
            .run(Delivery::Inherit)
        });
        Holder { held, ended }
    }

    fn answer(&self, delivery: &mut Delivery) -> io::Result<i32> {
        if let Answer::Exit(code) = self.attempt(delivery)? {
            return Ok(code);
        }
        let provision = Provision {
            session: self.session,
            live: self.live,
        };
        match provision.through_newest(delivery)? {
            Installed::Done => match self.attempt(delivery)? {
                Answer::Exit(code) => Ok(code),
                Answer::Missing => Ok(provision.failed(delivery)),
            },
            Installed::NoneThere => Ok(provision.none_there(delivery)),
            Installed::Failed | Installed::NoLockTaker | Installed::Unlockable => {
                Ok(provision.failed(delivery))
            }
            Installed::Unreached => Ok(SSH_FAILED),
        }
    }

    /// The process that reaches the runner, and how the runner is to watch for its caller. A hold
    /// is always watched, since its release comes down the channel.
    fn launch(&self) -> Launch {
        let hold = self.holding.is_some();
        let (mut command, watch) = match &self.session.route {
            Route::Here => (
                Runner::here(),
                Watch {
                    off: self.live.no_watchdog && !hold,
                    hold,
                    lease: 0,
                },
            ),
            Route::Ssh { host } => {
                let off = (self.live.no_live || self.live.no_watchdog) && !hold;
                let mut ssh = Command::new("ssh");
                ssh.args(Ssh::options()).arg(host).arg(Runner::far_line());
                let die_with_me = !self.live.no_pdeathsig;
                // SAFETY: the closure makes async-signal-safe calls only.
                unsafe {
                    ssh.pre_exec(move || {
                        if die_with_me {
                            parent_death_signal();
                        }
                        Ok(())
                    });
                }
                let lease = if off { 0 } else { self.live.lease };
                (ssh, Watch { off, hold, lease })
            }
        };
        if hold {
            // SAFETY: the closure makes async-signal-safe calls only.
            unsafe {
                command.pre_exec(|| {
                    libc::signal(libc::SIGINT, libc::SIG_IGN);
                    libc::signal(libc::SIGQUIT, libc::SIG_IGN);
                    Ok(())
                });
            }
        }
        Launch { command, watch }
    }

    fn attempt(&self, delivery: &mut Delivery) -> io::Result<Answer> {
        let Launch { mut command, watch } = self.launch();
        let lines = matches!(delivery, Delivery::Lines(_));
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        if lines {
            command.stderr(Stdio::piped());
        }
        let mut child = command.spawn()?;
        drop(command);
        let request = Frame::Request(Box::new(self.values.request(watch))).encode();
        let stdin = child.stdin.take().expect("stdin was piped");
        let (channel, messages) = mpsc::channel();
        let beat = (watch.lease > 0).then(|| Duration::from_millis(watch.lease * 250));
        thread::spawn(move || feed(stdin, request, beat, messages));
        let release = self.holding.as_ref().map(|_| channel.clone());

        let (tell, heard) = mpsc::channel();
        let out = child.stdout.take().expect("stdout was piped");
        let frames = {
            let tell = tell.clone();
            thread::spawn(move || read_frames(out, tell))
        };
        let err = child.stderr.take().map(|err| {
            let tell = tell.clone();
            thread::spawn(move || read_lines(err, tell))
        });
        drop(tell);
        let waiter = thread::spawn(move || {
            let status = child.wait();
            drop(channel);
            status
        });
        let mut exit = None;
        let mut any = false;
        let mut buffers = LineBuffers::default();
        for item in heard {
            any |= !matches!(item, Heard::Line(_));
            match item {
                Heard::Out(bytes) => delivery.give(Stream::Out, &bytes, &mut buffers),
                Heard::Line(bytes) => delivery.give(Stream::Err, &bytes, &mut buffers),
                Heard::Err(bytes) => delivery.give(Stream::Err, &bytes, &mut buffers),
                Heard::Holding(ports) => {
                    if let (Some(holding), Some(release)) = (&self.holding, &release) {
                        let _ = holding.send(Held {
                            ports,
                            release: Release(release.clone()),
                        });
                    }
                }
                Heard::Exit(code) => exit = Some(code),
                Heard::Broken(why) => {
                    delivery.say(&format!("dibs: the runner's answer broke off: {why}\n"))
                }
            }
        }
        drop(release);
        delivery.flush(&mut buffers);
        let _ = frames.join();
        if let Some(err) = err {
            let _ = err.join();
        }
        let status = waiter
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the wait for the runner panicked")))?;
        Ok(match exit {
            Some(code) => Answer::Exit(code),
            None if Interrupt::heard() => Answer::Exit(i32::from(Exit::Interrupted.code())),
            None if !any && exit_code(status) == MISSING => Answer::Missing,
            None => Answer::Exit(exit_code(status)),
        })
    }
}

/// How a call reaches the runner.
struct Launch {
    command: Command,
    watch: Watch,
}

/// How an attempt ended.
enum Answer {
    Exit(i32),
    /// The machine has no runner for this source.
    Missing,
}

/// Partial lines held back until their end arrives.
#[derive(Default)]
pub(super) struct LineBuffers {
    out: Vec<u8>,
    err: Vec<u8>,
}

impl Delivery<'_> {
    pub(super) fn give(&mut self, stream: Stream, bytes: &[u8], buffers: &mut LineBuffers) {
        match self {
            Delivery::Inherit => {
                let _ = match stream {
                    Stream::Out => io::stdout()
                        .write_all(bytes)
                        .and_then(|()| io::stdout().flush()),
                    Stream::Err => io::stderr().write_all(bytes),
                };
            }
            Delivery::Lines(on_line) => {
                let held = match stream {
                    Stream::Out => &mut buffers.out,
                    Stream::Err => &mut buffers.err,
                };
                held.extend_from_slice(bytes);
                let complete = held
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map_or(0, |at| at + 1);
                let lines: Vec<u8> = held.drain(..complete).collect();
                for line in lines.split_inclusive(|b| *b == b'\n') {
                    on_line(stream, line);
                }
            }
        }
    }

    /// What is left of a last line without its end.
    pub(super) fn flush(&mut self, buffers: &mut LineBuffers) {
        if let Delivery::Lines(on_line) = self {
            for (stream, held) in [(Stream::Out, &buffers.out), (Stream::Err, &buffers.err)] {
                if !held.is_empty() {
                    on_line(stream, held);
                }
            }
        }
    }

    pub fn say(&mut self, text: &str) {
        match self {
            Delivery::Inherit => eprint!("{text}"),
            Delivery::Lines(on_line) => {
                for line in text.split_inclusive('\n') {
                    on_line(Stream::Err, line.as_bytes());
                }
            }
        }
    }
}

/// The request, then a release when one is sent and a beat whenever so long passes without
/// one, until every sender is gone: the runner's end, and a hold's release.
fn feed(
    mut stdin: ChildStdin,
    request: Vec<u8>,
    beat: Option<Duration>,
    messages: Receiver<Message>,
) {
    if stdin.write_all(&request).is_err() {
        return;
    }
    let frames = std::iter::from_fn(|| {
        let next = match beat {
            Some(every) => messages.recv_timeout(every),
            None => messages.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match next {
            Ok(Message::Release(status)) => Some(Frame::Release(status)),
            Err(RecvTimeoutError::Timeout) => Some(Frame::Beat),
            Err(RecvTimeoutError::Disconnected) => None,
        }
    });
    for frame in frames {
        if stdin.write_all(&frame.encode()).is_err() {
            return;
        }
    }
}

/// Frames off the runner's stdout, each passed on as it is whole.
fn read_frames(mut out: impl Read, tell: mpsc::Sender<Heard>) {
    let mut unframer = Unframer::default();
    let mut chunk = [0u8; 8192];
    while let Ok(n @ 1..) = out.read(&mut chunk) {
        unframer.feed(&chunk[..n]);
        while let Some(frame) = match unframer.next_frame() {
            Ok(frame) => frame,
            Err(e) => {
                let _ = tell.send(Heard::Broken(e.to_string()));
                return;
            }
        } {
            let heard = match frame {
                Frame::Out(bytes) => Heard::Out(bytes),
                Frame::Err(bytes) => Heard::Err(bytes),
                Frame::Record(Record::Trailer(trailer)) => {
                    Heard::Err(format!("{trailer}\n").into_bytes())
                }
                Frame::Record(Record::Holding(ports)) => Heard::Holding(ports),
                Frame::Exit(code) => Heard::Exit(code),
                Frame::Request(_) | Frame::Beat | Frame::Release(_) => continue,
            };
            if tell.send(heard).is_err() {
                return;
            }
        }
    }
}

/// The runner's own stderr, and ssh's, a line at a time.
pub(super) fn read_lines(err: impl Read, tell: mpsc::Sender<Heard>) {
    let mut err = BufReader::new(err);
    let mut line = Vec::new();
    while matches!(err.read_until(b'\n', &mut line), Ok(1..)) {
        if tell.send(Heard::Line(std::mem::take(&mut line))).is_err() {
            return;
        }
    }
}
