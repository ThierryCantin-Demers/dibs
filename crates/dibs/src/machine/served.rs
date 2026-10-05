use crate::machine::{
    held::{Held, Holder, Release},
    interrupt::Interrupt,
    lines::{Listener, Stream},
    provision::{Installed, Provision},
    session::{Answer, Kept, Liveness, Message, Route, SSH_FAILED, Session, exit_code},
    ssh::{Ssh, parent_death_signal},
    values::{CallValues, Watch},
};
use dibs_format::{
    Exit,
    wire::{Frame, Picked, Prepared, Record, Unframer},
};
use std::{
    fs::File,
    io::{self, BufRead as _, BufReader, Read, Write},
    os::{
        fd::{AsFd as _, AsRawFd as _},
        unix::process::CommandExt as _,
    },
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
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
            "sh -c 'r=$HOME/.cache/dibs/runner/{hash}/dibs-runner; [ -x \"$r\" ] || exit {MISSING}; exec \"$r\" serve {hash}'",
            hash = Runner::HASH
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
        command.args([RUNNER_WORD, "serve", Runner::HASH]);
        command
    }
}

/// Where a runner call's output goes.
pub enum Delivery<'a> {
    /// This process's own streams.
    Inherit,
    /// Read here a line at a time.
    Lines(&'a mut dyn FnMut(Stream, &[u8])),
    /// Read here a line at a time, with the facts the runner tells besides.
    Listening(&'a mut dyn Listener),
}

/// A call the runner serves, from this side: the request goes in as a frame, and what comes back
/// is the call's output and its exit.
pub struct Served<'a> {
    pub session: &'a Session,
    pub values: &'a CallValues,
    pub live: Liveness,
    /// A hold's: told once the lock is held, with what releases it.
    pub holding: Option<Sender<Held>>,
    /// When an asked call is stopped, and whether it was.
    pub deadline: Option<Deadline>,
}

/// A bound on a call that is asked rather than run: past it, the call is stopped.
#[derive(Debug, Clone)]
pub struct Deadline {
    at: Instant,
    passed: Arc<AtomicBool>,
}

impl Deadline {
    pub fn after(bound: Duration) -> Deadline {
        Deadline {
            at: Instant::now() + bound,
            passed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn passed(&self) -> bool {
        self.passed.load(Ordering::SeqCst)
    }

    /// Stops the process at the deadline, unless `ended` says first that it has gone.
    fn watch(&self, pid: u32, ended: Receiver<()>) {
        let left = self.at.saturating_duration_since(Instant::now());
        let passed = Arc::clone(&self.passed);
        thread::spawn(move || {
            if let Err(RecvTimeoutError::Timeout) = ended.recv_timeout(left) {
                passed.store(true, Ordering::SeqCst);
                // SAFETY: the process is not reaped before `ended` closes.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            }
        });
    }
}

/// What reached this side from the runner.
pub(super) enum Heard {
    Out(Vec<u8>),
    Err(Vec<u8>),
    /// A line the runner or ssh wrote on stderr, outside any frame.
    Line(Vec<u8>),
    Holding(Vec<Picked>),
    Prepared(Box<Prepared>),
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
                deadline: None,
            }
            .run(Delivery::Inherit)
        });
        Holder { held, ended }
    }

    /// Runs the call and keeps what it prints; the exit is None when its deadline passed first.
    pub fn ask(&self, kept: Kept) -> io::Result<Answer> {
        let mut output = Vec::new();
        let mut on_line = |stream: Stream, bytes: &[u8]| match (stream, kept) {
            (Stream::Out, _) | (Stream::Err, Kept::Everything) => output.extend_from_slice(bytes),
            (Stream::Err, Kept::Stdout) => {
                let _ = io::stderr().write_all(bytes);
            }
            (Stream::Err, Kept::StdoutAlone) => {}
        };
        let exit = self.run(Delivery::Lines(&mut on_line))?;
        let passed = self.deadline.as_ref().is_some_and(Deadline::passed);
        Ok(Answer {
            output,
            exit: (!passed).then_some(exit),
        })
    }

    fn answer(&self, delivery: &mut Delivery) -> io::Result<i32> {
        if let Attempted::Exit(code) = self.attempt(delivery)? {
            return Ok(code);
        }
        let provision = Provision {
            session: self.session,
            live: self.live,
        };
        if self.deadline.is_some() {
            return Ok(provision.not_for_a_question(delivery));
        }
        match provision.through_newest(delivery)? {
            Installed::Done => match self.attempt(delivery)? {
                Attempted::Exit(code) => Ok(code),
                Attempted::Missing => Ok(provision.failed(delivery)),
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
                let die_with_me = true;
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

    /// rsync's far side: the request, then this process's own stdin and stdout carried raw both
    /// ways, but only once the runner says it has read the request, so a runner that is not
    /// there yet is built first without a byte of rsync's stream lost.
    pub fn transfer(&self, prepared: Option<&Path>) -> io::Result<i32> {
        let deferred = Interrupt::defer();
        let carried = match self.carry(prepared)? {
            Attempted::Missing => {
                let provision = Provision {
                    session: self.session,
                    live: self.live,
                };
                let delivery = &mut Delivery::Inherit;
                match provision.through_newest(delivery)? {
                    Installed::Done => match self.carry(prepared)? {
                        Attempted::Exit(code) => code,
                        Attempted::Missing => provision.failed(delivery),
                    },
                    Installed::NoneThere => provision.none_there(delivery),
                    Installed::Unreached => SSH_FAILED,
                    Installed::Failed | Installed::NoLockTaker | Installed::Unlockable => {
                        provision.failed(delivery)
                    }
                }
            }
            Attempted::Exit(code) => code,
        };
        drop(deferred);
        Ok(carried)
    }

    /// Frames until the runner says the transfer is under way: what it says on the way, and the
    /// tree it lays out first, which is written to `prepared`.
    fn carry(&self, prepared: Option<&Path>) -> io::Result<Attempted> {
        let Launch { mut command, .. } = self.launch();
        let watch = Watch {
            off: true,
            hold: false,
            lease: 0,
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        drop(command);
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let mut stdout = child.stdout.take().expect("stdout was piped");
        let request = Frame::Request(Box::new(self.values.request(watch))).encode();
        let said = stdin.write_all(&request).ok().and_then(|()| {
            let mut unframer = Unframer::default();
            let mut chunk = [0u8; 256];
            let mut last = None;
            while !matches!(
                last,
                Some(Frame::Record(Record::Transferring) | Frame::Exit(_))
            ) {
                let wanted = unframer.wanted().min(chunk.len());
                let n = stdout.read(&mut chunk[..wanted]).ok().filter(|n| *n > 0)?;
                unframer.feed(&chunk[..n]);
                let Some(frame) = unframer.next_frame().ok()? else {
                    continue;
                };
                match &frame {
                    Frame::Err(bytes) => {
                        let _ = io::stderr().write_all(bytes);
                    }
                    Frame::Record(Record::Prepared(laid)) => {
                        if let (Some(path), Ok(json)) = (prepared, serde_json::to_vec(laid)) {
                            let _ = std::fs::write(path, json);
                        }
                    }
                    _ => {}
                }
                last = Some(frame);
            }
            last
        });
        if let Some(Frame::Exit(code)) = said {
            drop(stdin);
            let status = child.wait()?;
            Interrupt::pass_on(status);
            return Ok(Attempted::Exit(code));
        }
        let Some(Frame::Record(Record::Transferring)) = said else {
            drop(stdin);
            let status = child.wait()?;
            Interrupt::pass_on(status);
            return Ok(match (said, exit_code(status)) {
                (None, MISSING) => Attempted::Missing,
                (_, code) => Attempted::Exit(code),
            });
        };
        // Unbuffered copies of this process's own: stdout's line buffering would hold rsync's
        // bytes back until a newline happened along. rsync leaves its transport's stdout
        // non-blocking, which a plain copy would take for the stream's end.
        let from = File::from(io::stdin().as_fd().try_clone_to_owned()?);
        let to = File::from(io::stdout().as_fd().try_clone_to_owned()?);
        for stream in [&from, &to] {
            let fd = stream.as_raw_fd();
            // SAFETY: fcntl reads and sets the flags of a descriptor owned here.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags >= 0 && flags & libc::O_NONBLOCK != 0 {
                    libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
                }
            }
        }
        thread::spawn(move || pass_through(from, stdin));
        pass_through(stdout, to);
        let status = child.wait()?;
        Interrupt::pass_on(status);
        Ok(Attempted::Exit(exit_code(status)))
    }

    fn attempt(&self, delivery: &mut Delivery) -> io::Result<Attempted> {
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
        let (gone, ended) = mpsc::channel::<()>();
        if let Some(deadline) = &self.deadline {
            deadline.watch(child.id(), ended);
        }
        let waiter = thread::spawn(move || {
            let status = child.wait();
            drop(channel);
            drop(gone);
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
                Heard::Prepared(prepared) => delivery.prepared(&prepared),
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
            Some(code) => Attempted::Exit(code),
            None if Interrupt::heard() => Attempted::Exit(i32::from(Exit::Interrupted.code())),
            None if !any && exit_code(status) == MISSING => Attempted::Missing,
            None => Attempted::Exit(exit_code(status)),
        })
    }
}

/// How a call reaches the runner.
struct Launch {
    command: Command,
    watch: Watch,
}

/// How an attempt ended.
enum Attempted {
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
        if let Delivery::Inherit = self {
            let _ = match stream {
                Stream::Out => io::stdout()
                    .write_all(bytes)
                    .and_then(|()| io::stdout().flush()),
                Stream::Err => io::stderr().write_all(bytes),
            };
            return;
        }
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
            self.line(stream, line);
        }
    }

    /// What is left of a last line without its end.
    pub(super) fn flush(&mut self, buffers: &mut LineBuffers) {
        for (stream, held) in [(Stream::Out, &buffers.out), (Stream::Err, &buffers.err)] {
            if !held.is_empty() {
                self.line(stream, held);
            }
        }
    }

    pub fn say(&mut self, text: &str) {
        match self {
            Delivery::Inherit => eprint!("{text}"),
            _ => {
                for line in text.split_inclusive('\n') {
                    self.line(Stream::Err, line.as_bytes());
                }
            }
        }
    }

    fn line(&mut self, stream: Stream, line: &[u8]) {
        match self {
            Delivery::Inherit => {}
            Delivery::Lines(on_line) => on_line(stream, line),
            Delivery::Listening(listener) => listener.line(stream, line),
        }
    }

    fn prepared(&mut self, prepared: &Prepared) {
        if let Delivery::Listening(listener) = self {
            listener.prepared(prepared);
        }
    }
}

/// Bytes from one stream to the other as they come, until either ends. Read and written plainly:
/// `io::copy` splices between a socket and a pipe, and rsync's first bytes never arrived that way.
fn pass_through(mut from: impl Read, mut to: impl Write) {
    let mut chunk = vec![0u8; 64 * 1024];
    while let Ok(n @ 1..) = from.read(&mut chunk) {
        if to.write_all(&chunk[..n]).is_err() {
            return;
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
                Frame::Record(Record::Prepared(prepared)) => Heard::Prepared(prepared),
                Frame::Exit(code) => Heard::Exit(code),
                Frame::Request(_)
                | Frame::Beat
                | Frame::Release(_)
                | Frame::Record(Record::Transferring) => continue,
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
