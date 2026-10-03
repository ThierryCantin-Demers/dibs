use std::process::{Command, Stdio};

const DEFAULT_CONNECT_TIMEOUT: &str = "10";

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
