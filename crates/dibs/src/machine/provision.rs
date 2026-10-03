use crate::machine::{
    lines::Stream,
    served::{Delivery, LineBuffers, MISSING, Runner},
    session::{Liveness, Route, SSH_FAILED, Session, Ssh, exit_code, parent_death_signal},
};
use dibs_format::Exit;
use std::{
    io::{self, BufRead as _, BufReader, Read, Write as _},
    os::unix::process::CommandExt as _,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
};

/// Puts the runner this binary was built with on a machine, built there from the source it
/// carries.
pub struct Provision<'a> {
    pub session: &'a Session,
    pub live: Liveness,
}

/// How putting the runner there went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Done,
    /// The machine has no runner at all to build it with.
    NoneThere,
    Failed,
    /// ssh could not reach it, which the call's own diagnosis explains.
    Unreached,
}

impl Provision<'_> {
    /// Built by the newest runner already there, as a shared job, unless it is there already.
    pub fn through_newest(&self, delivery: &mut Delivery) -> io::Result<Installed> {
        delivery.say(&format!(
            "dibs: {} has no runner for this dibs yet. Building it there as a shared job, once per version.\n",
            self.session.name
        ));
        self.install(&Provision::newest_line(), delivery)
    }

    /// `dibs --check`: the runner there, building the first one if the machine has none. No
    /// runner exists then to take the lock, so that one build runs outside it.
    pub fn ensure(&self, delivery: &mut Delivery) -> io::Result<Installed> {
        if self.session.route == Route::Here {
            return Ok(Installed::Done);
        }
        match self.install(&Provision::newest_line(), delivery)? {
            Installed::NoneThere => {
                delivery.say(&format!(
                    "dibs: installing dibs's runner on {}, a first build, which runs outside the lock because nothing there can take it yet.\n",
                    self.session.name
                ));
                self.install(&Provision::first_line(), delivery)
            }
            installed => Ok(installed),
        }
    }

    /// Says nothing ran because the build failed, and gives the exit for it.
    pub fn failed(&self, delivery: &mut Delivery) -> i32 {
        delivery.say(&format!(
            "dibs: the runner for this dibs could not be built on {}, so nothing ran. Its build said why, above.\n",
            self.session.name
        ));
        i32::from(Exit::NoRunner.code())
    }

    pub fn none_there(&self, delivery: &mut Delivery) -> i32 {
        delivery.say(&format!(
            "dibs: {} has no dibs runner yet, so nothing ran. Install one with:  dibs --check {}\n",
            self.session.name, self.session.name
        ));
        i32::from(Exit::NoRunner.code())
    }

    /// Exits 0 when the runner is there, and otherwise has the newest one build it.
    fn newest_line() -> String {
        let hash = Runner::HASH;
        format!(
            "sh -c 'd=$HOME/.cache/dibs/runner; [ -x \"$d/{hash}/dibs-runner\" ] && exit 0; r=$(ls -t \"$d\"/*/dibs-runner 2>/dev/null | head -n 1); [ -n \"$r\" ] || exit {MISSING}; exec \"$r\" build {hash}'"
        )
    }

    /// Unpacks the tree from stdin and runs its own install script.
    fn first_line() -> String {
        format!(
            "sh -c 'd=$HOME/.cache/dibs/runner; s=$d/.src.$$; rm -rf \"$s\" && mkdir -p \"$s\" && cd \"$s\" && tar -xzf - && PATH=$HOME/.cargo/bin:$PATH CARGO_TARGET_DIR=$d/.target sh install.sh {}; e=$?; cd / && rm -rf \"$s\"; exit $e'",
            Runner::HASH
        )
    }

    /// Runs a line there with the tree on its stdin, its output passed on as it comes.
    fn install(&self, line: &str, delivery: &mut Delivery) -> io::Result<Installed> {
        let Route::Ssh { host } = &self.session.route else {
            return Ok(Installed::Done);
        };
        let mut ssh = Command::new("ssh");
        ssh.args(Ssh::options()).arg(host).arg(line);
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
        ssh.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = ssh.spawn()?;
        drop(ssh);
        let mut stdin = child.stdin.take().expect("stdin was piped");
        thread::spawn(move || {
            let _ = stdin.write_all(Runner::SOURCE);
        });
        // The build's whole output goes to stderr: stdout is the command's, which has not run.
        let (tell, heard) = mpsc::channel();
        let pipes: [Option<Box<dyn Read + Send>>; 2] = [
            child
                .stdout
                .take()
                .map(|r| Box::new(r) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|r| Box::new(r) as Box<dyn Read + Send>),
        ];
        let readers: Vec<_> = pipes
            .into_iter()
            .flatten()
            .map(|pipe| {
                let tell = tell.clone();
                thread::spawn(move || {
                    let mut pipe = BufReader::new(pipe);
                    let mut line = Vec::new();
                    while matches!(pipe.read_until(b'\n', &mut line), Ok(1..)) {
                        if tell.send(std::mem::take(&mut line)).is_err() {
                            return;
                        }
                    }
                })
            })
            .collect();
        drop(tell);
        let mut buffers = LineBuffers::default();
        for line in heard {
            delivery.give(Stream::Err, &line, &mut buffers);
        }
        delivery.flush(&mut buffers);
        for reader in readers {
            let _ = reader.join();
        }
        Ok(match exit_code(child.wait()?) {
            0 => Installed::Done,
            MISSING => Installed::NoneThere,
            SSH_FAILED => Installed::Unreached,
            _ => Installed::Failed,
        })
    }
}
