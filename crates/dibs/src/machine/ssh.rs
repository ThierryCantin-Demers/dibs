use std::{
    path::Path,
    process::{Command, Stdio},
};

const DEFAULT_CONNECT_TIMEOUT: &str = "10";
/// What ssh says when the machine answered and the connection broke off before the login, as
/// sshd does to new connections while too many are starting at once.
const BROKE_OFF: [&str; 3] = [
    "during banner exchange",
    "kex_exchange_identification",
    "ssh_exchange_identification",
];
/// Seconds before each new try of a connection that broke off.
const DEFAULT_RECONNECT: [u64; 3] = [5, 15, 40];

/// How dibs runs ssh.
pub struct Ssh;

impl Ssh {
    pub fn connect_timeout() -> String {
        std::env::var("DIBS_CONNECT_TIMEOUT")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_CONNECT_TIMEOUT.into())
    }

    /// Whether ssh broke off before the login, which passes, rather than finding nothing there.
    pub fn broke_off(said: &str) -> bool {
        BROKE_OFF.iter().any(|needle| said.contains(needle))
    }

    /// The pauses before each new try of a connection that broke off, `DIBS_RECONNECT` in
    /// seconds separated by commas.
    pub fn reconnect_pauses() -> Vec<u64> {
        std::env::var("DIBS_RECONNECT")
            .ok()
            .and_then(|v| v.split(',').map(|p| p.trim().parse().ok()).collect())
            .unwrap_or_else(|| DEFAULT_RECONNECT.to_vec())
    }

    /// ssh, reading `config` alone when the machine names one: given `-F`, ssh reads neither the
    /// user's file nor the system's, so a machine's own file has to describe all of it.
    pub fn to(config: Option<&Path>) -> Command {
        let mut ssh = Command::new("ssh");
        if let Some(config) = config {
            ssh.arg("-F").arg(config);
        }
        ssh
    }

    /// No TTY, so nothing downstream believes it is interactive, and a machine that stops
    /// answering is given up on after about two minutes.
    pub fn options() -> Vec<String> {
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
    pub fn dials(host: &str, config: Option<&Path>) -> Option<String> {
        let out = Ssh::to(config)
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

    /// The kernel signals the child when this process dies, SIGKILL included.
    #[cfg(target_os = "linux")]
    pub fn parent_death_signal() {
        // SAFETY: prctl with these arguments only sets a flag on the calling process.
        unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) };
    }

    #[cfg(not(target_os = "linux"))]
    pub fn parent_death_signal() {}

    /// The host of an ssh destination: the part after its last `@`.
    pub fn host_of(destination: &str) -> &str {
        destination.rsplit('@').next().unwrap_or(destination)
    }
}
