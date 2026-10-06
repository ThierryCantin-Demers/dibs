use crate::{
    call::{
        base::{CallError, Fingerprint, LockedCall},
        machine::{Asked, MachineCall},
        origin::{Origin, RecipeJob},
        output::Output,
    },
    caller::Caller,
    cli::{BashQuoted, Call, Command as Words},
    machine::{CallValues, Interrupt, Lines, Liveness, Relayed, Route, Session, Target},
    paths::Paths,
    scratch::ScratchFile,
};
use dibs_format::{
    Exit, Label, Mode,
    wire::{Prepared, Tree},
};
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
    /// A recipe's sync is laid out by the machine ahead of the transfer, under the same lock,
    /// from its job's tree.
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
        let command = Words(vec![format!("rsync {}", quoted.join(" "))]);
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
        output.say(&format!(
            "dibs: syncing with {}{named} once its lock is held; the files are read then, not now\n",
            target.host
        ));
        // SAFETY: the closure makes an async-signal-safe call only.
        unsafe {
            rsync.pre_exec(|| {
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let deferred = Interrupt::defer();
        if !matches!(output, Output::Inherit) {
            rsync.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        let mut child = rsync.spawn()?;
        let relayed = Relayed::to(child.id());
        match output {
            Output::Lines(on_line) => Lines::of(&mut child).relay(*on_line),
            Output::Listening(listener) => {
                Lines::of(&mut child).relay(&mut |stream, line| listener.line(stream, line))
            }
            Output::Inherit => {}
        }
        let status = child.wait();
        if let Some(prepared) = transport.prepared() {
            output.prepared(&prepared);
        }
        drop(deferred);
        let status = Exit::shell_status(status?);
        let exit = match (status, transport.exit()) {
            (0, _) => 0,
            (_, Some(code @ (64..=78))) => code,
            (status, _) => status,
        };
        drop(transport);
        relayed.pass_on();
        Ok(exit)
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
/// reads the tree to lay out from, and writes its exit and the tree it laid out to, removed when
/// the sync ends.
struct Transport<'a> {
    sync: &'a Sync<'a>,
    exit: PathBuf,
    tree: Option<TreeFiles>,
}

/// The tree a transport asks the machine to lay out, and where it writes what was laid out.
struct TreeFiles {
    asked: PathBuf,
    prepared: PathBuf,
}

impl<'a> Transport<'a> {
    fn create(sync: &'a Sync<'a>) -> io::Result<Transport<'a>> {
        let dir = Paths::from_env().scratch().ok_or_else(|| {
            io::Error::other("no DIBS_SCRATCH and no HOME to keep a sync's files in")
        })?;
        let mut transport = Transport {
            sync,
            exit: ScratchFile::create(&dir, ".rsh-exit", b"")?,
            tree: None,
        };
        if let Origin::Recipe(RecipeJob {
            tree: Some(tree), ..
        }) = sync.origin
        {
            let asked = serde_json::to_vec(tree).map_err(io::Error::other)?;
            transport.tree = Some(TreeFiles {
                asked: ScratchFile::create(&dir, ".rsh-tree", &asked)?,
                prepared: ScratchFile::create(&dir, ".rsh-prepared", b"")?,
            });
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
        if let Some(tree) = &self.tree {
            words.extend([
                Rsh::TREE.to_string(),
                tree.asked.display().to_string(),
                Rsh::PREPARED.to_string(),
                tree.prepared.display().to_string(),
            ]);
        }
        words.extend([Rsh::EXIT.to_string(), self.exit.display().to_string()]);
        Ok(words
            .iter()
            .map(|w| Rsync::word(w))
            .collect::<Vec<_>>()
            .join(" "))
    }

    /// The tree the machine laid out, as the transport wrote it down.
    fn prepared(&self) -> Option<Prepared> {
        let tree = self.tree.as_ref()?;
        serde_json::from_slice(&std::fs::read(&tree.prepared).ok()?).ok()
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
        if let Some(tree) = &self.tree {
            let _ = std::fs::remove_file(&tree.asked);
            let _ = std::fs::remove_file(&tree.prepared);
        }
    }
}

/// The machine's end of a sync, which rsync starts as its transport: `dibs __rsh`, its own
/// words, then rsync's `[-l <user>] <host> <command...>`.
pub struct Rsh {
    stream: bool,
    label: Option<Label>,
    fingerprint: Option<String>,
    tree: Option<PathBuf>,
    prepared: Option<PathBuf>,
    exit: Option<PathBuf>,
    command: Vec<String>,
}

impl Rsh {
    /// The word rsync starts a transport with, outside the grammar.
    pub const WORD: &'static str = "__rsh";
    const STREAM: &'static str = "--stream";
    const LABEL: &'static str = "--label";
    const TREE: &'static str = "--tree";
    const PREPARED: &'static str = "--prepared";
    const FINGERPRINT: &'static str = "--fingerprint";
    const EXIT: &'static str = "--exit";
    const FLAGS: [&'static str; 6] = [
        Rsh::STREAM,
        Rsh::LABEL,
        Rsh::TREE,
        Rsh::PREPARED,
        Rsh::FINGERPRINT,
        Rsh::EXIT,
    ];

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
            fingerprint: None,
            tree: None,
            prepared: None,
            exit: None,
            command: Vec::new(),
        };
        let mut words = words.iter().map(String::as_str).peekable();
        while let Some(flag) = words.next_if(|w| Rsh::FLAGS.contains(w)) {
            match flag {
                Rsh::STREAM => rsh.stream = true,
                Rsh::LABEL => rsh.label = Some(Label::new(words.next()?)),
                Rsh::FINGERPRINT => rsh.fingerprint = Some(words.next()?.to_string()),
                Rsh::TREE => rsh.tree = Some(PathBuf::from(words.next()?)),
                Rsh::PREPARED => rsh.prepared = Some(PathBuf::from(words.next()?)),
                Rsh::EXIT => rsh.exit = Some(PathBuf::from(words.next()?)),
                _ => return None,
            }
        }
        if words.next()? == "-l" {
            words.nth(1)?;
        }
        rsh.command = words.map(str::to_string).collect();
        Some(rsh)
    }

    fn transfer(&self, caller: &Caller) -> Result<i32, CallError> {
        let tree: Option<Tree> = match &self.tree {
            Some(path) => Some(
                std::fs::read(path)
                    .and_then(|asked| serde_json::from_slice(&asked).map_err(io::Error::other))
                    .map_err(|e| {
                        CallError::Io(io::Error::other(format!("{}: {e}", path.display())))
                    })?,
            ),
            None => None,
        };
        let call = Call {
            stream: self.stream,
            ..Call::default()
        };
        let machine = MachineCall::new(&call, caller)?;
        let command = self.command.join(" ");
        let label = self.label.clone().unwrap_or_else(|| Label::new(SYNC_LABEL));
        let target = machine.target()?;
        let session = Session::new(&target, &machine.here);
        if session.inside_hold() {
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
            fingerprint: self.fingerprint.as_deref().map(|f| Fingerprint(f).sent()),
            tree,
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
        let status = session.transfer(&values, Liveness::from_env(), self.prepared.as_deref())?;
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
    use std::{
        fs,
        io::Write as _,
        process::{Command, Stdio},
    };

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn rsync_3_is_found_past_an_older_one_and_an_older_one_alone_is_refused() {
        let dir = std::env::temp_dir().join(format!("dibs-rsync-test.{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // Written by a process of its own: a fork from a parallel test would hold this one's
        // write descriptor across the exec of it, which then fails with ETXTBSY.
        let fake = |name: &str, says: &str| {
            let path = dir.join(name);
            let mut writer = Command::new("sh")
                .args(["-c", "cat > \"$0\" && chmod 755 \"$0\""])
                .arg(&path)
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            let script = format!("#!/bin/sh\nprintf '{says}'\n");
            let mut stdin = writer.stdin.take().unwrap();
            stdin.write_all(script.as_bytes()).unwrap();
            drop(stdin);
            assert!(writer.wait().unwrap().success());
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
