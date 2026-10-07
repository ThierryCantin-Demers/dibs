use crate::machine::{
    lines::Stream,
    served::{Delivery, LineBuffers, MISSING, Runner},
    session::{Liveness, Route, SSH_FAILED, Session},
    ssh::Ssh,
};
use dibs_format::Exit;
use dibs_runner::BUILD_MAX;
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
    /// No perl there to take the lock for a first build, so nothing was built.
    NoLockTaker,
    /// Its lock directory cannot be written.
    Unlockable,
}

/// What the first build's line exits with when it cannot take the lock.
const UNLOCKABLE: i32 = 71;
const NO_LOCK_TAKER: i32 = 73;

/// The first build, as `sh -c` reads it with the hash and the cap as `$1` and `$2`: the lock
/// directory found as the runner finds it, then the build lock every build of the runner takes
/// first, the gate and `rw`, all through perl's `flock`, which is `flock(2)` as the runner's is.
/// Perl holds `rw`, close-on-exec, for as long as the build runs in a process group of its own,
/// which it stops on TERM, HUP or INT, at the cap, or when its caller goes. Fish reads it
/// inside single quotes too, so it holds no single quote and no doubled
/// backslash.
const FIRST_BUILD: &str = r#"h=$1 m=$2 d=$HOME/.cache/dibs/runner
command -v perl >/dev/null 2>&1 || exit 73
sd=${DIBS_SHARED_LOCK_DIR:-/dev/shm/dibs-lock}
if [ -n "${DIBS_LOCK_DIR:-}" ]; then l=$DIBS_LOCK_DIR
elif [ -d "$sd" ] && [ -w "$sd" ]; then l=$sd; umask 002
else l=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/dibs-lock; fi
mkdir -p "$l" 2>/dev/null || { l=/tmp/dibs-lock-$(id -u); mkdir -p "$l"; }
: > "$l/.writable.$$" 2>/dev/null || exit 71
rm -f "$l/.writable.$$"
mkdir -p "$d" || exit 1
exec perl /dev/fd/3 "$l" "$d" "$h" "$m" 3<<"PERL"
use Fcntl qw(:DEFAULT :flock);
use POSIX qw(:sys_wait_h setpgid);
use IO::Poll qw(POLLERR POLLHUP);
use File::Path qw(remove_tree);
my ($l, $d, $h, $m) = @ARGV;
sub record {
    open(my $f, ">", "$l/waiting.$$") or return;
    print $f join(chr(9), "shared", $$, time, "dibs-runner", "dibs --check", "", "-", "the first build of dibs-runner $h", ""), chr(10);
    close($f);
}
sub unlockable { unlink("$l/waiting.$$"); exit 71; }
open(my $one, ">>", "$d/.build.lock") or exit 1;
unless (flock($one, LOCK_EX | LOCK_NB)) {
    print STDERR "dibs: another build of dibs-runner is running there; this one waits for it.", chr(10);
    flock($one, LOCK_EX) or exit 1;
}
open(my $g, ">>", "$l/gate") or exit 71;
open(my $r, ">>", "$l/rw") or exit 71;
record();
flock($g, LOCK_EX) or unlockable();
unless (flock($r, LOCK_SH | LOCK_NB)) {
    print STDERR "dibs: the machine is busy, so the first build waits for its shared lock.", chr(10);
    flock($r, LOCK_SH) or unlockable();
}
close($g);
record();
rename("$l/waiting.$$", "$l/holder.$$");
my $pid = fork();
unless (defined($pid)) { unlink("$l/holder.$$"); exit 1; }
if ($pid == 0) {
    setpgid(0, 0);
    exec("sh", "-c", q{exec 3<&-; s=$1/.src.$$
rm -rf "$s" && mkdir -p "$s" && cd "$s" && tar -xmzf - && PATH=$HOME/.cargo/bin:$PATH CARGO_TARGET_DIR=$1/.target sh install.sh "$2"}, "sh", $d, $h);
    POSIX::_exit(127);
}
setpgid($pid, $pid);
my ($stop, $at) = ("", 0);
sub stop { ($stop, $at) = ($_[0], time) unless $stop; kill($_[1], -$pid); }
$SIG{TERM} = sub { stop("TERM", "TERM") };
$SIG{HUP} = sub { stop("HUP", "HUP") };
$SIG{INT} = sub { stop("INT", "INT") };
$SIG{ALRM} = sub { stop("ALRM", "TERM") };
$SIG{CHLD} = sub {};
alarm($m);
my $out = IO::Poll->new;
$out->mask(*STDOUT => POLLERR | POLLHUP);
while (waitpid($pid, WNOHANG) == 0) {
    $out->poll(1);
    if ($out->events(*STDOUT)) { $out->remove(*STDOUT); stop("gone", "TERM"); }
    kill("KILL", -$pid) if $stop && time - $at > 30;
}
my $st = $?;
alarm(0);
kill("KILL", -$pid);
remove_tree("$d/.src.$pid");
unlink("$l/holder.$$");
print STDERR "dibs: the first build of dibs-runner ran past $m seconds, so it was stopped.", chr(10) if $stop eq "ALRM";
my %code = (TERM => 143, HUP => 129, INT => 130, ALRM => 124, gone => 1);
exit($code{$stop}) if $stop;
exit(($st & 127) ? 128 + ($st & 127) : $st >> 8);
PERL
"#;

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
    /// runner exists then to take the lock, so that build takes it through perl.
    pub fn ensure(&self, delivery: &mut Delivery) -> io::Result<Installed> {
        if self.session.route == Route::Here {
            return Ok(Installed::Done);
        }
        match self.install(&Provision::newest_line(), delivery)? {
            Installed::NoneThere => {
                delivery.say(&format!(
                    "dibs: installing dibs's runner on {}, a first build, as a shared job.\n",
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

    /// Says why a first build could not take the lock, and gives the exit for it.
    pub fn unlocked(&self, installed: Installed, delivery: &mut Delivery) -> i32 {
        let name = &self.session.name;
        match installed {
            Installed::NoLockTaker => {
                delivery.say(&format!(
                    "dibs: {name} has no perl, which takes the lock for the first build of dibs's runner, so nothing was built.\n  \
                     A build outside the lock would run beside whatever is measured there. Install perl, then run dibs --check {name} again.\n"
                ));
                i32::from(Exit::NoRunner.code())
            }
            _ => {
                delivery.say(&format!(
                    "dibs: the lock directory on {name} cannot be written, so the first build of dibs's runner took no lock and nothing was built.\n"
                ));
                i32::from(Exit::NoLock.code())
            }
        }
    }

    pub fn none_there(&self, delivery: &mut Delivery) -> i32 {
        delivery.say(&format!(
            "dibs: {} has no dibs runner yet, so nothing ran. Install one with:  dibs --check {}\n",
            self.session.name, self.session.name
        ));
        i32::from(Exit::NoRunner.code())
    }

    /// A question asked within a bound builds nothing: the build would queue behind whatever
    /// holds the machine, for as long as that takes.
    pub fn not_for_a_question(&self, delivery: &mut Delivery) -> i32 {
        let name = &self.session.name;
        delivery.say(&format!(
            "dibs: {name} has no runner for this dibs yet, so it was not asked. A call that runs there builds it first, and so does:  dibs --check {name}\n"
        ));
        i32::from(Exit::NoRunner.code())
    }

    /// Exits 0 when the runner is there, and otherwise has the newest one build it.
    fn newest_line() -> String {
        let hash = Runner::HASH;
        format!(
            "sh -c 'd=$HOME/.cache/dibs/runner; [ \"$(\"$d/{hash}/dibs-runner\" hash 2>/dev/null)\" = {hash} ] && exit 0; r=$(ls -t \"$d\"/*/dibs-runner 2>/dev/null | head -n 1); [ -n \"$r\" ] || exit {MISSING}; exec \"$r\" build {hash}'"
        )
    }

    /// Takes the shared lock, then unpacks the tree from stdin and runs its own install script.
    fn first_line() -> String {
        format!("sh -c '{FIRST_BUILD}' sh {} {BUILD_MAX}", Runner::HASH)
    }

    /// Runs a line there with the tree on its stdin, its output passed on as it comes.
    fn install(&self, line: &str, delivery: &mut Delivery) -> io::Result<Installed> {
        let Route::Ssh { host } = &self.session.route else {
            return Ok(Installed::Done);
        };
        let mut ssh = Command::new("ssh");
        ssh.args(Ssh::options()).arg(host).arg(line);
        let die_with_me = true;
        // SAFETY: the closure makes async-signal-safe calls only.
        unsafe {
            ssh.pre_exec(move || {
                if die_with_me {
                    Ssh::parent_death_signal();
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
        Ok(match Exit::shell_status(child.wait()?) {
            0 => Installed::Done,
            MISSING => Installed::NoneThere,
            SSH_FAILED => Installed::Unreached,
            NO_LOCK_TAKER => Installed::NoLockTaker,
            UNLOCKABLE => Installed::Unlockable,
            _ => Installed::Failed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_build_reads_the_same_inside_single_quotes_in_every_shell() {
        assert!(!FIRST_BUILD.contains('\''));
        assert!(!FIRST_BUILD.contains("\\\\"));
        assert!(FIRST_BUILD.contains(&format!("exit {NO_LOCK_TAKER}")));
        assert!(FIRST_BUILD.contains(&format!("exit {UNLOCKABLE}")));
    }

    #[test]
    fn the_runner_source_resolves_against_its_own_lock_file() {
        let dir = std::env::temp_dir().join(format!("dibs-runner-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut tar = std::process::Command::new("tar")
            .args(["xzf", "-", "-C"])
            .arg(&dir)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut tar.stdin.take().unwrap(), Runner::SOURCE).unwrap();
        assert!(tar.wait().unwrap().success());
        // The cargo running this test, since the one on PATH may queue behind it.
        let out = std::process::Command::new(env!("CARGO"))
            .args(["metadata", "--locked", "--offline", "--format-version", "1"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
