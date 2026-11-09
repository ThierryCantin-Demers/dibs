use std::{
    path::PathBuf,
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

/// What ssh is told about a machine beyond the name it dials.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dial {
    /// The ssh configuration the machine names, in place of the user's own.
    pub config: Option<PathBuf>,
    pub port: Option<u16>,
}

impl Dial {
    /// The ssh command a person types to reach `host`.
    pub fn typed(&self, host: &str) -> String {
        match self.port {
            Some(port) => format!("ssh -p {port} {host}"),
            None => format!("ssh {host}"),
        }
    }

    /// How `known_hosts` names `host`, which on another port is not the name alone.
    pub fn known_as(&self, host: &str) -> String {
        match self.port {
            Some(port) => format!("'[{host}]:{port}'"),
            None => host.to_string(),
        }
    }
}

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

    /// ssh, reading the config alone when the machine names one: given `-F`, ssh reads neither
    /// the user's file nor the system's, so a machine's own file has to describe all of it.
    pub fn to(dial: &Dial) -> Command {
        let mut ssh = Command::new("ssh");
        if let Some(config) = &dial.config {
            ssh.arg("-F").arg(config);
        }
        if let Some(port) = dial.port {
            ssh.arg("-p").arg(port.to_string());
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
    pub fn dials(host: &str, dial: &Dial) -> Option<String> {
        let out = Ssh::to(dial)
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

    /// `user@host:406` as what ssh dials and the port, which ssh would not read from it. An IPv6
    /// address has colons too, and what follows its first one is never a number.
    pub fn and_port(address: &str) -> (&str, Option<u16>) {
        match address
            .split_once(':')
            .map(|(host, port)| (host, port.parse()))
        {
            Some((host, Ok(port))) => (host, Some(port)),
            _ => (address, None),
        }
    }

    /// The host of an ssh destination: the part after its last `@`.
    pub fn host_of(destination: &str) -> &str {
        destination.rsplit('@').next().unwrap_or(destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_number_after_the_one_colon_is_a_port() {
        assert_eq!(Ssh::and_port("me@box:406"), ("me@box", Some(406)));
        assert_eq!(Ssh::and_port("me@box"), ("me@box", None));
        assert_eq!(Ssh::and_port("::1"), ("::1", None));
        assert_eq!(Ssh::and_port("fe80::1:2"), ("fe80::1:2", None));
        assert_eq!(Ssh::and_port("box:notaport"), ("box:notaport", None));
    }
}
