use crate::{
    call::{
        base::{CallError, Fingerprint, LockedCall, holding},
        machine::{Asked, MachineCall},
        origin::{Origin, RecipeJob},
        output::Output,
    },
    caller::Caller,
    cli::{BashQuoted, Call, Command as Words},
    machine::{CallValues, Interrupt, Lines, Liveness, Route, Session, Target, exit_code},
    paths::Paths,
    scratch::ScratchFile,
};
use dibs_format::{Exit, Label, Mode};
use std::{
    fmt, io,
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::{Command, Stdio},
};

const SYNC_LABEL: &str = "sync";

/// `dibs --sync`: rsync between here and a machine, under the machine's shared lock.
pub struct Sync<'a> {
    pub machine: &'a MachineCall<'a>,
    pub args: &'a [String],
    /// Lines the machine runs ahead of the transfer, under the same lock.
    pub before: &'a str,
    pub origin: Origin<'a>,
}

impl Sync<'_> {
    pub fn answer(&self) -> Result<i32, CallError> {
        self.answer_into(&mut Output::Inherit)
    }

    /// Transfers, with what rsync and the machine say going where `output` says.
    pub fn answer_into(&self, output: &mut Output) -> Result<i32, CallError> {
        let target = self.machine.target()?;
        self.machine.somewhere(&target)?;
        let marked = |arg: &String| match arg.strip_prefix(':') {
            Some(path) => format!("{}:{path}", target.host),
            None => arg.clone(),
        };
        let mut args: Vec<String> = self.args.iter().map(marked).collect();
        let into_machine = args
            .last()
            .is_some_and(|last| last.starts_with(&format!("{}:", target.host)));
        if into_machine && preserves_mtimes(&args) {
            output.say(
                "dibs: --sync is preserving mtimes into the machine. A build there may then compile nothing:\n  \
                 for a source tree use --checksum --no-times instead of -a.\n",
            );
        }
        let rsync = match Rsync::find() {
            Ok(rsync) => rsync,
            Err(missing) => {
                output.say(&missing.to_string());
                return Ok(i32::from(Exit::Refused.code()));
            }
        };
        if rsync.mkpath() && !args.iter().any(|a| a == "--mkpath" || a == "--no-mkpath") {
            args.insert(0, "--mkpath".into());
        }
        match Session::new(&target, &self.machine.here).route {
            Route::Here => self.here(&target, &args, output),
            Route::Ssh { .. } => self.over_rsync(&rsync, &target, &args, output),
        }
    }

    /// On the machine itself a copy is a copy: an ordinary shared job, which competes for
    /// bandwidth like any other.
    fn here(
        &self,
        target: &Target,
        args: &[String],
        output: &mut Output,
    ) -> Result<i32, CallError> {
        let machine_side = format!("{}:", target.host);
        let quoted: Vec<String> = args
            .iter()
            .map(|a| BashQuoted(a.strip_prefix(&machine_side).unwrap_or(a)).to_string())
            .collect();
        let script = format!("{}rsync {}", Before(self.before), quoted.join(" "));
        let command = Words(vec![script]);
        let call = Call {
            label: Some(self.label()),
            ..self.machine.call.clone()
        };
        LockedCall::shared(&command).made_by(self.origin).run_into(
            &call,
            self.machine.caller,
            output,
        )
    }

    /// rsync runs here and reaches the machine through `dibs __rsh`, which takes the lock.
    fn over_rsync(
        &self,
        found: &Rsync,
        target: &Target,
        args: &[String],
        output: &mut Output,
    ) -> Result<i32, CallError> {
        let transport = Transport::create(self)?;
        let mut rsync = Command::new(&found.path);
        rsync
            .arg("-e")
            .arg(transport.words()?)
            .args(args)
            .env("DIBS_HOST", &target.host)
            .env("DIBS_HOSTNAME", &target.hostname);
        if let Some(machine) = &target.machine {
            rsync.env("DIBS_ON", machine.as_str());
        }
        if let Origin::Recipe(job) = self.origin {
            rsync.stdin(Stdio::null());
            if let Some(batch) = &job.batch {
                rsync.envs(batch.vars());
            }
        }
        let named = match &target.machine {
            Some(machine) if machine.as_str() != target.host => format!(" ({machine})"),
            _ => String::new(),
        };
        output.say(&format!("dibs: syncing with {}{named}\n", target.host));
        // SAFETY: the closure makes an async-signal-safe call only.
        unsafe {
            rsync.pre_exec(|| {
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let deferred = Interrupt::defer();
        let status = match output {
            Output::Inherit => rsync.status(),
            Output::Lines(on_line) => rsync
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    Lines::of(&mut child).relay(*on_line);
                    child.wait()
                }),
        };
        drop(deferred);
        let status = exit_code(status?);
        Ok(match (status, transport.exit()) {
            (0, _) => 0,
            (_, Some(code @ (64..=78))) => code,
            (status, _) => status,
        })
    }

    fn label(&self) -> Label {
        self.machine
            .call
            .label
            .clone()
            .unwrap_or_else(|| Label::new(SYNC_LABEL))
    }
}

/// What a sync hands the transport rsync starts, which is a process of its own: the files it
/// reads its lines ahead from and writes its exit to, removed when the sync ends.
struct Transport<'a> {
    sync: &'a Sync<'a>,
    exit: PathBuf,
    before: Option<PathBuf>,
}

impl<'a> Transport<'a> {
    fn create(sync: &'a Sync<'a>) -> io::Result<Transport<'a>> {
        let dir = Paths::from_env().scratch().ok_or_else(|| {
            io::Error::other("no DIBS_SCRATCH and no HOME to keep a sync's files in")
        })?;
        let mut transport = Transport {
            sync,
            exit: ScratchFile::create(&dir, ".rsh-exit", b"")?,
            before: None,
        };
        if !sync.before.is_empty() {
            transport.before = Some(ScratchFile::create(
                &dir,
                ".rsh-before",
                sync.before.as_bytes(),
            )?);
        }
        Ok(transport)
    }

    /// rsync's `-e`, which it splits on whitespace and follows with the host and its command.
    fn words(&self) -> io::Result<String> {
        let me = std::env::current_exe()?;
        let mut words = vec![me.display().to_string(), Rsh::WORD.to_string()];
        if self.sync.machine.call.stream {
            words.push(Rsh::STREAM.into());
        }
        if let Some(label) = &self.sync.machine.call.label {
            words.extend([Rsh::LABEL.to_string(), label.to_string()]);
        }
        if let Origin::Recipe(RecipeJob {
            fingerprint: Some(fingerprint),
            ..
        }) = self.sync.origin
        {
            words.extend([
                Rsh::FINGERPRINT.to_string(),
                Fingerprint(fingerprint).sent(),
            ]);
        }
        if let Some(before) = &self.before {
            words.extend([Rsh::BEFORE.to_string(), before.display().to_string()]);
        }
        words.extend([Rsh::EXIT.to_string(), self.exit.display().to_string()]);
        Ok(words
            .iter()
            .map(|w| Rsync::word(w))
            .collect::<Vec<_>>()
            .join(" "))
    }

    fn exit(&self) -> Option<i32> {
        std::fs::read_to_string(&self.exit)
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl Drop for Transport<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.exit);
        if let Some(before) = &self.before {
            let _ = std::fs::remove_file(before);
        }
    }
}

/// Lines run ahead of a transfer, each ending in a newline.
struct Before<'a>(&'a str);

impl fmt::Display for Before<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.trim_end_matches('\n') {
            "" => Ok(()),
            lines => writeln!(f, "{lines}"),
        }
    }
}

/// The machine's end of a sync, which rsync starts as its transport: `dibs __rsh`, its own
/// words, then rsync's `[-l <user>] <host> <command...>`.
pub struct Rsh {
    stream: bool,
    label: Option<Label>,
    fingerprint: String,
    before: Option<PathBuf>,
    exit: Option<PathBuf>,
    command: Vec<String>,
}

impl Rsh {
    /// The word rsync starts a transport with, outside the grammar.
    pub const WORD: &'static str = "__rsh";
    const STREAM: &'static str = "--stream";
    const LABEL: &'static str = "--label";
    const BEFORE: &'static str = "--before";
    const FINGERPRINT: &'static str = "--fingerprint";
    const EXIT: &'static str = "--exit";

    /// Transfers, and writes its exit where the sync that started rsync reads it, since rsync
    /// may report the transport's failure as a stream error of its own.
    pub fn serve(words: &[String]) -> i32 {
        let Some(rsh) = Rsh::read(words) else {
            eprintln!(
                "dibs: {} is rsync's transport, not for calling directly",
                Rsh::WORD
            );
            return i32::from(Exit::Refused.code());
        };
        let caller = Caller::from_env();
        let exit = rsh.transfer(&caller).unwrap_or_else(|e| {
            eprint!("{e}");
            e.exit()
        });
        if let Some(file) = &rsh.exit {
            let _ = std::fs::write(file, format!("{exit}\n"));
        }
        exit
    }

    fn read(words: &[String]) -> Option<Rsh> {
        let mut rsh = Rsh {
            stream: false,
            label: None,
            fingerprint: String::new(),
            before: None,
            exit: None,
            command: Vec::new(),
        };
        let mut words = words.iter();
        let mut rest = loop {
            match words.next()?.as_str() {
                Rsh::STREAM => rsh.stream = true,
                Rsh::LABEL => rsh.label = Some(Label::new(words.next()?)),
                Rsh::FINGERPRINT => rsh.fingerprint = words.next()?.clone(),
                Rsh::BEFORE => rsh.before = Some(PathBuf::from(words.next()?)),
                Rsh::EXIT => rsh.exit = Some(PathBuf::from(words.next()?)),
                first => break std::iter::once(first).chain(words.map(String::as_str)),
            }
        };
        let first = rest.next()?;
        if first == "-l" {
            rest.nth(1)?;
        }
        rsh.command = rest.map(str::to_string).collect();
        Some(rsh)
    }

    fn transfer(&self, caller: &Caller) -> Result<i32, CallError> {
        let before = match &self.before {
            Some(path) => std::fs::read_to_string(path)
                .map_err(|e| CallError::Io(io::Error::other(format!("{}: {e}", path.display()))))?,
            None => String::new(),
        };
        let call = Call {
            stream: self.stream,
            ..Call::default()
        };
        let machine = MachineCall::new(&call, caller)?;
        let command = format!("{}{}", Before(&before), self.command.join(" "));
        let label = self.label.clone().unwrap_or_else(|| Label::new(SYNC_LABEL));
        let target = machine.target()?;
        let session = Session::new(&target, &machine.here);
        if holding(&session.lock_at) {
            return Err(CallError::InsideHold {
                lock_at: session.lock_at,
            });
        }
        if session.route == Route::Here {
            eprintln!("dibs: --sync reaches the machine from elsewhere. You are on it: use cp.");
            return Ok(i32::from(Exit::Refused.code()));
        }
        machine.somewhere(&target)?;
        let values = CallValues {
            fingerprint: Fingerprint(&self.fingerprint).sent(),
            ..machine.values(
                Asked {
                    mode: Mode::Rsh,
                    label,
                    command,
                    streamed: false,
                },
                &target,
            )?
        };
        let status = exit_code(session.transfer(&values, Liveness::from_env())?);
        Ok(session.exit(status, &target))
    }
}

/// The rsync on this computer.
/// The rsync a sync runs here: the first rsync 3 on PATH or where Homebrew puts it. macOS's own
/// is 2.6.9 or openrsync, which lack what a sync relies on, so it is never settled for.
struct Rsync {
    path: PathBuf,
    version: RsyncVersion,
}

/// What stands in for rsync 3 here, when nothing does.
struct NoRsync {
    older: Option<Rsync>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RsyncVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Rsync {
    const PLACES: [&'static str; 3] = ["rsync", "/opt/homebrew/bin/rsync", "/usr/local/bin/rsync"];
    const MKPATH_SINCE: RsyncVersion = RsyncVersion {
        major: 3,
        minor: 2,
        patch: 3,
    };

    fn find() -> Result<Rsync, NoRsync> {
        Rsync::first_of(&Rsync::PLACES)
    }

    fn first_of(places: &[&str]) -> Result<Rsync, NoRsync> {
        let mut older = None;
        for &place in places {
            let Some(version) = RsyncVersion::of(place) else {
                continue;
            };
            let found = Rsync {
                path: PathBuf::from(place),
                version,
            };
            match version.major >= 3 {
                true => return Ok(found),
                false => {
                    older.get_or_insert(found);
                }
            }
        }
        Err(NoRsync { older })
    }

    fn mkpath(&self) -> bool {
        self.version >= Rsync::MKPATH_SINCE
    }

    /// A word of `-e`'s command, which rsync splits on whitespace.
    fn word(text: &str) -> String {
        match text.contains(char::is_whitespace) {
            true => format!("'{}'", text.replace('\'', r"'\''")),
            false => text.to_string(),
        }
    }
}

impl RsyncVersion {
    /// What `<rsync> --version` says it is; None when it does not run.
    fn of(rsync: &str) -> Option<RsyncVersion> {
        let out = Command::new(rsync)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        RsyncVersion::read(&String::from_utf8_lossy(&out.stdout))
    }

    /// The first line naming rsync's version: `rsync  version 3.2.7  protocol version 31`, or
    /// openrsync's `rsync version 2.6.9 compatible` under a line of its own.
    fn read(text: &str) -> Option<RsyncVersion> {
        let line = text
            .lines()
            .find(|l| l.starts_with("rsync") && l.contains(" version "))?;
        let number = line.split(" version ").nth(1)?.split_whitespace().next()?;
        let mut parts = number
            .split(|c: char| !c.is_ascii_digit())
            .map(|p| p.parse().ok());
        Some(RsyncVersion {
            major: parts.next()??,
            minor: parts.next().flatten().unwrap_or(0),
            patch: parts.next().flatten().unwrap_or(0),
        })
    }
}

impl fmt::Display for RsyncVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl fmt::Display for NoRsync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(older) = &self.older else {
            return writeln!(f, "--sync needs rsync on both machines");
        };
        writeln!(
            f,
            "dibs: --sync needs rsync 3 here, and the only rsync found is {} {}, which is too old: macOS ships one.",
            older.path.display(),
            older.version
        )?;
        writeln!(f, "  Install rsync 3 with:  brew install rsync")
    }
}

/// Whether rsync is asked to carry mtimes across: `-a`, `-t`, `--archive`, `--times`, or a word of
/// short flags with an `a` anywhere after its first letter, later words included.
fn preserves_mtimes(args: &[String]) -> bool {
    let joined = format!(" {} ", args.join(" "));
    if joined.contains(" --no-times ") || joined.contains(" --no-t ") {
        return false;
    }
    let cluster = joined.match_indices(" -").any(|(at, _)| {
        let rest = &joined.as_bytes()[at + 2..];
        rest.first().is_some_and(u8::is_ascii_alphabetic) && rest[1..].contains(&b'a')
    });
    [" -a ", " -t ", " --archive ", " --times "]
        .iter()
        .any(|flag| joined.contains(flag))
        || cluster
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn rsync_3_is_found_past_an_older_one_and_an_older_one_alone_is_refused() {
        let dir = std::env::temp_dir().join(format!("dibs-rsync-test.{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fake = |name: &str, says: &str| {
            let path = dir.join(name);
            fs::write(&path, format!("#!/bin/sh\nprintf '{says}'\n")).unwrap();
            fs::set_permissions(&path, PermissionsExt::from_mode(0o755)).unwrap();
            path.display().to_string()
        };
        let apple = fake(
            "apple",
            "openrsync: protocol version 29\nrsync version 2.6.9 compatible\n",
        );
        let brew = fake("brew", "rsync  version 3.2.7  protocol version 31\n");
        let found = Rsync::first_of(&["/nonexistent/rsync", &apple, &brew])
            .ok()
            .unwrap();
        assert_eq!(
            (found.path.display().to_string(), found.mkpath()),
            (brew, true)
        );
        let refused = Rsync::first_of(&[&apple]).err().unwrap().to_string();
        assert!(
            refused.contains("2.6.9") && refused.contains("brew install rsync"),
            "{refused}"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_rsync_is_known_by_the_version_it_names() {
        let read = |text: &str| RsyncVersion::read(text).map(|v| v.to_string());
        assert_eq!(
            read("rsync  version 3.5.0-g483b5efc  protocol version 32"),
            Some("3.5.0".into())
        );
        assert_eq!(
            read("rsync  version 2.6.9  protocol version 29"),
            Some("2.6.9".into())
        );
        assert_eq!(read("usage: something else"), None);
    }

    #[test]
    fn mtimes_are_read_from_the_flags_as_the_glob_read_them() {
        assert!(preserves_mtimes(&words("-a ./x m:~/y")));
        assert!(preserves_mtimes(&words("-va ./x m:~/y")));
        assert!(!preserves_mtimes(&words("-av ./x m:~/y")));
        assert!(preserves_mtimes(&words("--times ./x m:~/y")));
        assert!(!preserves_mtimes(&words("-a --no-times ./x m:~/y")));
        assert!(!preserves_mtimes(&words("--checksum ./x m:~/y")));
        assert!(!preserves_mtimes(&words("-r ./x m:~/y")));
    }
}
