use dibs_format::wire::{Prepared, Stepped};
use std::{
    io::{BufRead as _, BufReader, Read},
    process::{Child, ChildStderr, ChildStdout},
    sync::mpsc,
};

/// What reads a call's output a line at a time, and the facts its runner tells besides it.
pub trait Listener {
    fn line(&mut self, stream: Stream, bytes: &[u8]);

    /// The job's tree, laid out ahead of its command.
    fn prepared(&mut self, prepared: &Prepared);

    /// What was done around a recipe step's command.
    fn stepped(&mut self, stepped: &Stepped);
}

/// Which of a child's streams a line came on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

/// A child's stdout and stderr, read a line at a time in the order they arrive.
pub struct Lines {
    out: Option<ChildStdout>,
    err: Option<ChildStderr>,
}

impl Lines {
    /// The child's piped streams, taken from it so it can be waited for elsewhere.
    pub fn of(child: &mut Child) -> Lines {
        Lines {
            out: child.stdout.take(),
            err: child.stderr.take(),
        }
    }

    /// Hands each line to `on_line`, on this thread, until both streams close.
    pub fn relay(self, on_line: &mut dyn FnMut(Stream, &[u8])) {
        let out = self
            .out
            .map(|s| (Stream::Out, Box::new(s) as Box<dyn Read + Send>));
        let err = self
            .err
            .map(|s| (Stream::Err, Box::new(s) as Box<dyn Read + Send>));
        let (tx, rx) = mpsc::channel::<(Stream, Vec<u8>)>();
        let readers: Vec<_> = [out, err]
            .into_iter()
            .flatten()
            .map(|(stream, pipe)| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let mut pipe = BufReader::new(pipe);
                    let mut line = Vec::new();
                    while matches!(pipe.read_until(b'\n', &mut line), Ok(1..)) {
                        if tx.send((stream, std::mem::take(&mut line))).is_err() {
                            break;
                        }
                    }
                })
            })
            .collect();
        drop(tx);
        for (stream, line) in rx {
            on_line(stream, &line);
        }
        for reader in readers {
            let _ = reader.join();
        }
    }
}
