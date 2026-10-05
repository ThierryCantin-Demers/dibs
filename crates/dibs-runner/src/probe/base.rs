use crate::{
    clock::Moment,
    machine::{Machine, Scope},
    platform::{Host, Platform as _},
    probe::Gpus,
    settings::{home, refused, unknown},
    sink::Sink,
    stop::Signals,
};
use std::{
    ffi::CString,
    fmt::Write as _,
    fs,
    os::unix::ffi::OsStrExt as _,
    path::Path,
    process::{Command, Stdio},
};

/// Marks the inventory entry `--check --write` records, which the client cuts out.
const ENTRY_START: &str = "--8<-- dibs inventory --8<--";
const ENTRY_END: &str = "--8<-- end --8<--";

/// `dibs --check`: what would otherwise fail later and further away. Reaching it at all proved
/// that ssh got there, that the login shell parsed the bootstrap and that this dibs's runner is
/// installed.
pub struct Probe<'a> {
    pub machine: &'a Machine,
    /// Print the inventory entry for the client to record.
    pub write: bool,
}

/// The report as it is written, and how many things failed or want a look.
#[derive(Default)]
pub struct Report {
    text: String,
    failed: usize,
    warned: usize,
}

impl Report {
    pub fn ok(&mut self, said: &str) {
        let _ = writeln!(self.text, "  ok    {said}");
    }

    pub fn warn(&mut self, said: &str) {
        let _ = writeln!(self.text, "  warn  {said}");
        self.warned += 1;
    }

    pub fn bad(&mut self, said: &str) {
        let _ = writeln!(self.text, "  FAIL  {said}");
        self.failed += 1;
    }

    pub fn note(&mut self, said: &str) {
        let _ = writeln!(self.text, "        {said}");
    }

    pub fn line(&mut self, said: &str) {
        let _ = writeln!(self.text, "{said}");
    }
}

impl Probe<'_> {
    /// Prints the report; 1 when something blocks the machine from being used.
    pub fn serve(&self, sink: &Sink) -> i32 {
        let machine = self.machine;
        let mut report = Report::default();
        report.line(&format!("dibs --check on {}", machine.host));
        report.line("");
        report.ok("this dibs's runner is installed, and the login shell started it");
        match first_line("bash", &["--version"]).and_then(|l| bash_version(&l)) {
            Some(version) => report.ok(&format!("bash {version}, which every job runs under")),
            None => report.bad("no bash, and every job runs under bash -c"),
        }
        let rsync = first_line("rsync", &["--version"])
            .and_then(|l| {
                let words: Vec<&str> = l.split_whitespace().collect();
                (words.first() == Some(&"rsync")).then(|| words.get(2).map(|v| v.to_string()))?
            })
            .filter(|v| v.starts_with(|c: char| ('3'..='9').contains(&c)));
        match rsync {
            Some(version) => report.ok(&format!("rsync {version}, so a tree can be sent here")),
            None => {
                report.bad("no rsync 3, so a tree sent from another computer cannot arrive");
                report.note("openrsync, which macOS ships as rsync, takes too few of its options.");
            }
        }
        report
            .ok("the CPU of reaped children is counted, so idle detection sees a job's whole tree");
        for refused in refused() {
            report.bad(&refused.to_string());
        }
        for unknown in unknown() {
            report.warn(&unknown.to_string());
        }
        self.lock_dir(&mut report);
        self.scratch(&mut report);
        let history = &machine.history;
        match history.starts_with(&machine.shared_state) {
            true => report.ok(&format!(
                "history and log shared: {}",
                history.parent().unwrap_or(history).display()
            )),
            false => {
                report.warn(&format!("history is per-user: {}", history.display()));
                report.note("Estimates are built only from your own runs, and the log shows only");
                report.note(
                    "your own jobs. Shared, as root:  install -d -m 2775 -g dibs /var/lib/dibs",
                );
            }
        }
        report.line("");
        let repos = clones();
        match repos.is_empty() {
            false => report.line(&format!(
                "  repos it can build:  {}",
                repos.iter().map(|r| format!("{r} ")).collect::<String>()
            )),
            true => {
                report.warn("no clones under ~/prog, so no recipe can be run here at all");
                report.note(
                    "It is dropped from routing for every repo until one is cloned. Anything",
                );
                report.note("this machine should build needs a clone at ~/prog/<repo> first.");
            }
        }
        report.line("");
        report.line("  devices");
        let model = Host::cpu_model().unwrap_or_else(|| "unknown".to_string());
        let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
        report.line(&format!("    cpu   {model}, {cores} threads"));
        let gpus = Gpus::find(&mut report);
        if self.write {
            report.line("");
            report.line(ENTRY_START);
            report.line("[machine.@NAME@]");
            report.line("ssh      = \"@SSH@\"");
            report.line(&format!("hostname = \"{}\"", machine.host));
            report.line(&format!("probed   = \"{}\"", Moment::now().day()));
            if Host::on_battery() {
                report
                    .line("measure  = false        # runs on a battery, so it throttles and moves");
                report.line(
                    "workstation = true      # someone works here; drop this if it is headless",
                );
            }
            report.line("\n  [[machine.@NAME@.device]]");
            report.line("  kind  = \"cpu\"");
            report.line(&format!("  name  = \"{model}\""));
            report.line(&format!("  cores = {cores}"));
            report.text.push_str(&gpus.entries());
            report.line(ENTRY_END);
        }
        report.line("");
        let code = match (report.failed, report.warned) {
            (0, 0) => {
                report.line("  ready.");
                0
            }
            (0, warned) => {
                report.line(&format!("  usable, {warned} thing(s) to look at."));
                0
            }
            (failed, warned) => {
                report.line(&format!(
                    "  {failed} blocking, {warned} to look at. This machine is not ready."
                ));
                1
            }
        };
        sink.out(report.text.as_bytes());
        code
    }

    fn lock_dir(&self, report: &mut Report) {
        let machine = self.machine;
        let dir = machine.lock_dir.display();
        match machine.scope {
            Scope::Shared => report.ok(&format!("lock directory is shared: {dir}")),
            Scope::Explicit => report.warn(&format!(
                "lock directory set explicitly: {dir} (only what set it will agree)"
            )),
            Scope::PerUser => {
                report.warn(&format!("lock directory is keyed to this uid: {dir}"));
                report
                    .note("Anyone logging in as a different user takes a different lock, and both");
                report.note(
                    "are told the machine is idle. Fine if everyone shares this account, and",
                );
                report.note("a wrong answer with nothing to notice it by if they do not.");
                report.note("");
                let shared = machine.shared_lock_dir.display();
                match cfg!(target_os = "linux") {
                    true => {
                        report.note("Either give everyone this one account, which also lets one build cache");
                        report.note("serve all of them, or make the lock directory shared. As root, idle:");
                        report.note("  groupadd -f dibs && gpasswd -a <each-user> dibs");
                        report.note(&format!("  install -d -m 2775 -g dibs {shared}"));
                        report.note(&format!(
                            "  printf 'd {shared} 2775 root dibs -\\n' > /etc/tmpfiles.d/dibs.conf"
                        ));
                        report.note("The last line recreates it on boot, since that tmpfs is emptied then.");
                    }
                    false => report.note(
                        "Give everyone this one account, which also lets one build cache serve all of them.",
                    ),
                }
            }
        }
    }

    /// Where builds and logs go, and whether a new tree there can start from a sibling's build.
    /// Its size is not measured: that reads every cache with no lock, beside whatever runs, and
    /// `dibs --gc --dry-run` says it under the shared lock.
    fn scratch(&self, report: &mut Report) {
        let scratch = &self.machine.scratch;
        let kind = Host::filesystem(scratch).unwrap_or_default();
        if !writable(scratch) {
            report.bad(&format!("scratch {} is not writable", scratch.display()));
        } else if kind == "tmpfs" {
            report.bad(&format!(
                "scratch {} is tmpfs, which is RAM and usually under a quota",
                scratch.display()
            ));
            report.note("One build tree there fills it for every user, and a full tmpfs breaks");
            report.note("every command including the ones for finding out why. Set DIBS_SCRATCH.");
        } else {
            report.ok(&format!(
                "scratch {} on {kind}, {} free",
                scratch.display(),
                available(scratch)
            ));
        }
        let target = scratch.join("target");
        if !target.is_dir() || !writable(&target) {
            return;
        }
        let probe = target.join(".dibs-reflink-check");
        let copy = target.join(".dibs-reflink-check.copy");
        let cloned = fs::write(&probe, "x\n").is_ok() && Host::reflink(&probe, &copy);
        let _ = fs::remove_file(&probe);
        let _ = fs::remove_file(&copy);
        if !cloned {
            report.warn(
                "no reflinks where target directories live, so every new tree builds from nothing",
            );
            report.note(&format!(
                "XFS or btrfs under {} lets a new tree start from a sibling's build for free.",
                target.display()
            ));
            return;
        }
        let kind = Host::filesystem(&target).unwrap_or_default();
        let caches = fs::read_dir(&target)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .count();
        report.ok(&format!(
            "target directories on {kind} with reflinks: a new tree starts from a copy of its repo's latest"
        ));
        report.note(&format!(
            "{caches} of them; dibs --gc --dry-run says what they hold, under the shared lock."
        ));
        if kind == "xfs" {
            report.note("df is no better a measure: XFS reserves about 2% of the disk up front for reflinks.");
        }
    }
}

/// `5.2.26` of `GNU bash, version 5.2.26(1)-release (...)`.
fn bash_version(line: &str) -> Option<String> {
    let after = line.split_once("version ")?.1;
    let version: String = after
        .chars()
        .take_while(|c| *c != '(' && *c != ' ')
        .collect();
    (!version.is_empty()).then_some(version)
}

/// What a tool printed first, None when it could not run.
pub fn first_line(program: &str, args: &[&str]) -> Option<String> {
    output_of(program, args)?.lines().next().map(str::to_string)
}

/// What a tool printed on stdout, None when it could not run or printed nothing.
pub fn output_of(program: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    Signals::unblocked(&mut command);
    let out = command.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

/// Whether a program is on `PATH`.
#[cfg(target_os = "linux")]
pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            fs::metadata(dir.join(program)).is_ok_and(|m| {
                std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
            })
        })
    })
}

fn writable(dir: &Path) -> bool {
    CString::new(dir.as_os_str().as_bytes()).is_ok_and(|path| {
        // SAFETY: access only reads the path.
        dir.is_dir() && unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0
    })
}

/// Free space as `df -h` gives it.
fn available(dir: &Path) -> String {
    let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
        return "?".into();
    };
    // SAFETY: statvfs is plain data, which statvfs fills from a NUL-terminated path.
    let mut found: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut found) } != 0 {
        return "?".into();
    }
    crate::gc::human(found.f_bavail as u64 * found.f_frsize as u64)
}

/// The repos under `~/prog` a worktree can be prepared from.
fn clones() -> Vec<String> {
    let mut repos: Vec<String> = fs::read_dir(home().join("prog"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join(".git").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    repos.sort();
    repos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_says_its_version_before_its_build() {
        assert_eq!(
            bash_version("GNU bash, version 5.2.26(1)-release (x86_64-redhat-linux-gnu)")
                .as_deref(),
            Some("5.2.26")
        );
        assert_eq!(bash_version("no such thing"), None);
    }
}
