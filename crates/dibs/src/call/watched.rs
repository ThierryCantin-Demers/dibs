//! A process that learns the moment the dibs that started it is gone, however it goes: it holds
//! the read end of a pipe only that dibs writes to, and reads end of file once that dibs exits.

use std::{
    fs::File,
    io::{self, PipeWriter},
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::process::CommandExt as _,
    },
    process::{Child, ChildStderr, ChildStdout, Command, ExitStatus},
};

/// Where the started process finds its end of the pipe.
const STARTER_FD: libc::c_int = 3;

/// A process started with its end of the pipe, and this side's end. Dropping it closes this
/// side's end, which the process reads as this dibs gone, so it is kept whole for as long as the
/// process should run.
pub struct Watched {
    child: Child,
    _alive: PipeWriter,
}

impl Watched {
    pub fn spawn(mut command: Command) -> io::Result<Watched> {
        let (read, write) = io::pipe()?;
        let raw = read.as_raw_fd();
        // SAFETY: dup2 and fcntl are async-signal-safe, and touch only the child's descriptors.
        unsafe {
            command.pre_exec(move || {
                match raw == STARTER_FD {
                    true => close_on_exec(raw, false),
                    false if libc::dup2(raw, STARTER_FD) < 0 => {
                        return Err(io::Error::last_os_error());
                    }
                    false => {}
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        Ok(Watched {
            child,
            _alive: write,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }
}

/// The started process's side of the pipe, kept from whatever it starts in turn.
pub struct Starter {
    end: File,
}

impl Starter {
    /// Takes this process's end of the pipe; only a process `Watched` started has one.
    pub fn take() -> Starter {
        close_on_exec(STARTER_FD, true);
        // SAFETY: the descriptor was set up by `Watched::spawn` and nothing else here uses it.
        let end = unsafe { File::from_raw_fd(STARTER_FD) };
        Starter { end }
    }

    /// Returns once the dibs that started this process is gone.
    pub fn gone(mut self) {
        let _ = io::copy(&mut self.end, &mut io::sink());
    }
}

fn close_on_exec(fd: libc::c_int, on: bool) {
    // SAFETY: fcntl on a descriptor this process holds changes only its flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            let flags = match on {
                true => flags | libc::FD_CLOEXEC,
                false => flags & !libc::FD_CLOEXEC,
            };
            libc::fcntl(fd, libc::F_SETFD, flags);
        }
    }
}
