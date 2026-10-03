use std::{
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const DEFAULT_CONNECT_TIMEOUT: &str = "10";
/// After the far line's `&&`, so the line a machine receives stays the same byte for byte.
const CONTINUATION: &str = "             ";

/// How dibs runs ssh.
pub struct Ssh;

impl Ssh {
    pub fn connect_timeout() -> String {
        std::env::var("DIBS_CONNECT_TIMEOUT")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_CONNECT_TIMEOUT.into())
    }

    /// No TTY, so nothing downstream believes it is interactive, and a machine that stops
    /// answering is given up on after about two minutes.
    pub(crate) fn options() -> Vec<String> {
        [
            "BatchMode=yes".to_string(),
            "LogLevel=ERROR".into(),
            format!("ConnectTimeout={}", Ssh::connect_timeout()),
            "ServerAliveInterval=30".into(),
            "ServerAliveCountMax=4".into(),
        ]
        .into_iter()
        .flat_map(|o| ["-o".to_string(), o])
        .collect()
    }

    /// Where the script is written there; unexpanded, for the far shell's own `$HOME`.
    pub fn remote_dir() -> Option<String> {
        std::env::var("DIBS_REMOTE_DIR")
            .ok()
            .filter(|v| !v.is_empty())
    }

    /// The line the login shell there runs, which fish and bash read alike: the script is read
    /// by length off stdin, and what follows it is the channel.
    pub(crate) fn far_line(count: usize) -> String {
        let dir = Ssh::remote_dir().unwrap_or_else(|| "$HOME/.cache/dibs/run".into());
        let script = format!(
            "{dir}/.dibs-payload.{}.{}.sh",
            std::process::id(),
            Ssh::stamp()
        );
        let trace = match std::env::var("DIBS_TRACE") {
            Ok(v) if !v.is_empty() => "-x",
            _ => "",
        };
        format!(
            "mkdir -p {dir} 2>/dev/null; dd bs=1 count={count} 2>/dev/null | base64 -d | gzip -dc > {script} && {CONTINUATION}exec bash {trace} {script}\nexit 70"
        )
    }

    /// The time in nanoseconds, never the same twice in this process, whose calls to machines that
    /// share a home would otherwise write one script over another.
    fn stamp() -> u64 {
        static LAST: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or_default();
        let next = |last: u64| now.max(last.saturating_add(1));
        let mut last = LAST.load(Ordering::Relaxed);
        while let Err(seen) =
            LAST.compare_exchange_weak(last, next(last), Ordering::Relaxed, Ordering::Relaxed)
        {
            last = seen;
        }
        next(last)
    }

    /// The address ssh dials for a host, which a held command reaches its services at.
    pub fn dials(host: &str) -> Option<String> {
        let out = Command::new("ssh")
            .args(["-G", host])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("hostname ").map(str::to_string))
            .filter(|h| !h.is_empty())
    }
}

/// The kernel signals the child when this process dies, SIGKILL included.
#[cfg(target_os = "linux")]
pub(crate) fn parent_death_signal() {
    // SAFETY: prctl with these arguments only sets a flag on the calling process.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) };
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn parent_death_signal() {}
