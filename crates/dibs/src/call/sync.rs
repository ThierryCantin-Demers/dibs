use crate::{
    call::{
        base::{CallError, LockedCall, holding},
        machine::{Asked, MachineCall},
    },
    cli::{Call, Command as Words},
    machine::{Interrupt, Liveness, MachineHalf, Route, Session, Target, exit_code},
};
use dibs_format::{Exit, Label, Mode};
use std::{
    io::Write as _,
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::{Command, Stdio},
};

const SYNC_LABEL: &str = "sync";

/// `dibs --sync`: rsync between here and a machine, under the machine's shared lock.
pub struct Sync<'a> {
    pub machine: &'a MachineCall<'a>,
    pub args: &'a [String],
}

/// The machine's end of a sync, which rsync starts as its transport: `dibs --rsh`.
pub struct Rsh<'a> {
    pub machine: &'a MachineCall<'a>,
    pub command: &'a [String],
}

impl Sync<'_> {
    pub fn answer(&self) -> Result<i32, CallError> {
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
            eprintln!(
                "dibs: --sync is preserving mtimes into the machine. A build there may then compile nothing:"
            );
            eprintln!("  for a source tree use --checksum --no-times instead of -a.");
        }
        let Some(mkpath) = Rsync::mkpath() else {
            eprintln!("--sync needs rsync on both machines");
            return Ok(i32::from(Exit::Refused.code()));
        };
        if mkpath && !args.iter().any(|a| a == "--mkpath" || a == "--no-mkpath") {
            args.insert(0, "--mkpath".into());
        }
        match Session::new(&target, &self.machine.here).route {
            Route::Here => self.here(&target, &args),
            Route::Ssh { .. } => self.over_rsync(&target, &args),
        }
    }

    /// On the machine itself a copy is a copy: an ordinary shared job, which competes for
    /// bandwidth like any other.
    fn here(&self, target: &Target, args: &[String]) -> Result<i32, CallError> {
        let machine_side = format!("{}:", target.host);
        let quoted: Vec<String> = args
            .iter()
            .map(|a| crate::cli::BashQuoted(a.strip_prefix(&machine_side).unwrap_or(a)).to_string())
            .collect();
        let before = match SyncBefore::read() {
            Ok(before) => before,
            Err(refused) => return Ok(refused),
        };
        let script = format!("{before}rsync {}", quoted.join(" "));
        let command = Words(vec![script]);
        let call = Call {
            label: Some(
                self.machine
                    .call
                    .label
                    .clone()
                    .unwrap_or_else(|| Label::new(SYNC_LABEL)),
            ),
            ..self.machine.call.clone()
        };
        LockedCall::shared(&command).run(&call, self.machine.caller)
    }

    /// rsync runs here and reaches the machine through `dibs --rsh`, which takes the lock.
    fn over_rsync(&self, target: &Target, args: &[String]) -> Result<i32, CallError> {
        let me = std::env::current_exe()?;
        let exit_file = RshExit::create()?;
        let mut rsync = Command::new("rsync");
        rsync
            .arg("-e")
            .arg(format!("{} --rsh", Rsync::word(&me.display().to_string())))
            .args(args)
            .env("DIBS_HOST", &target.host)
            .env("DIBS_HOSTNAME", &target.hostname)
            .env(
                "DIBS_SYNC_LABEL",
                self.machine
                    .call
                    .label
                    .as_ref()
                    .map(Label::as_str)
                    .unwrap_or_default(),
            )
            .env("DIBS_RSH_EXIT", &exit_file.0);
        if let Some(machine) = &target.machine {
            rsync.env("DIBS_ON", machine.as_str());
        }
        let named = match &target.machine {
            Some(machine) if machine.as_str() != target.host => format!(" ({machine})"),
            _ => String::new(),
        };
        eprintln!("dibs: syncing with {}{named}", target.host);
        // SAFETY: the closure makes an async-signal-safe call only.
        unsafe {
            rsync.pre_exec(|| {
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let deferred = Interrupt::defer();
        let status = rsync.status();
        drop(deferred);
        let status = exit_code(status?);
        let transport = exit_file.read();
        Ok(match (status, transport) {
            (0, _) => 0,
            (_, Some(code @ (64..=78))) => code,
            (status, _) => status,
        })
    }
}

impl Rsh<'_> {
    /// Writes its exit where the `--sync` that started rsync reads it, since rsync may report
    /// the transport's failure as a stream error of its own.
    pub fn answer(&self) -> Result<i32, CallError> {
        let exit = self.transfer();
        if let Some(file) = std::env::var_os("DIBS_RSH_EXIT").filter(|f| !f.is_empty()) {
            let code = exit.as_ref().map_or_else(CallError::exit, |code| *code);
            let _ = std::fs::write(file, format!("{code}\n"));
        }
        exit
    }

    fn transfer(&self) -> Result<i32, CallError> {
        let before = match SyncBefore::read() {
            Ok(before) => before,
            Err(refused) => return Ok(refused),
        };
        let command = format!("{before}{}", self.command.join(" "));
        let label = self.machine.call.label.clone().unwrap_or_else(|| {
            Label::new(
                std::env::var("DIBS_SYNC_LABEL")
                    .ok()
                    .filter(|l| !l.is_empty())
                    .unwrap_or_else(|| SYNC_LABEL.into()),
            )
        });
        let target = self.machine.target()?;
        let session = Session::new(&target, &self.machine.here);
        if holding(&session.lock_at) {
            return Err(CallError::InsideHold {
                lock_at: session.lock_at,
            });
        }
        if session.route == Route::Here {
            eprintln!("dibs: --sync reaches the machine from elsewhere. You are on it: use cp.");
            return Ok(i32::from(Exit::Refused.code()));
        }
        self.machine.somewhere(&target)?;
        let values = self.machine.values(
            Asked {
                mode: Mode::Rsh,
                label,
                command,
                streamed: false,
            },
            &target,
        )?;
        if self.machine.call.preflight {
            return Ok(0);
        }
        let half = MachineHalf::load()?;
        let status = exit_code(session.transfer(&values, &half, Liveness::from_env())?);
        Ok(session.exit(status, &target))
    }
}

/// What the recipe layer runs on the machine ahead of a transfer, from `DIBS_SYNC_BEFORE`.
struct SyncBefore;

impl SyncBefore {
    /// Its lines, ending in a newline, or the exit for a file that cannot be read.
    fn read() -> Result<String, i32> {
        let Some(path) = std::env::var_os("DIBS_SYNC_BEFORE").filter(|p| !p.is_empty()) else {
            return Ok(String::new());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) if !text.is_empty() => Ok(match text.trim_end_matches('\n') {
                "" => String::new(),
                lines => format!("{lines}\n"),
            }),
            _ => {
                eprintln!(
                    "dibs: DIBS_SYNC_BEFORE names {}, which cannot be read",
                    PathBuf::from(path).display()
                );
                Err(i32::from(Exit::Refused.code()))
            }
        }
    }
}

/// The rsync on this computer.
struct Rsync;

impl Rsync {
    /// Whether it takes `--mkpath`; None when there is no rsync.
    fn mkpath() -> Option<bool> {
        let out = Command::new("rsync")
            .arg("--help")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).contains("--mkpath"))
    }

    /// A word of `-e`'s command, which rsync splits on whitespace.
    fn word(text: &str) -> String {
        match text.contains(char::is_whitespace) {
            true => format!("'{}'", text.replace('\'', r"'\''")),
            false => text.to_string(),
        }
    }
}

/// A file the transport writes its exit to.
struct RshExit(PathBuf);

impl RshExit {
    fn create() -> std::io::Result<RshExit> {
        let dir = std::env::var_os("TMPDIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let path = dir.join(format!("dibs-rsh.{}", std::process::id()));
        std::fs::File::create(&path)?.flush()?;
        Ok(RshExit(path))
    }

    fn read(&self) -> Option<i32> {
        std::fs::read_to_string(&self.0).ok()?.trim().parse().ok()
    }
}

impl Drop for RshExit {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Whether rsync is asked to carry mtimes across, read as the bash client's glob read it.
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

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
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
