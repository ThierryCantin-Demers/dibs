use crate::machine::{
    ssh::Ssh,
    target::{Named, Target, after_at},
};
use std::{
    fmt::Write as _,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

/// Why ssh could not reach a machine, asked again of ssh, since what it said the first time went
/// to the terminal and was not kept.
pub struct Unreachable<'a> {
    pub target: &'a Target,
}

impl Unreachable<'_> {
    pub fn diagnosis(&self) -> String {
        let host = &self.target.host;
        let timeout = Ssh::connect_timeout();
        let secs = timeout.parse().unwrap_or(10);
        let why = bounded(
            Command::new("ssh").args([
                "-o",
                "BatchMode=yes",
                "-o",
                "LogLevel=ERROR",
                "-o",
                &format!("ConnectTimeout={timeout}"),
                host,
                "true",
            ]),
            secs,
        );
        let mut text = format!("dibs: cannot reach '{host}' over ssh.\n");
        match (self.target.named, &self.target.machine) {
            (Named::DibsHost, _) => {
                text.push_str("  Nothing on this call named a machine, so it went to DIBS_HOST.\n");
                text.push_str(
                    "  Name one with --on, or export DIBS_ON once to cover every call in a script.\n",
                );
            }
            (Named::Placed | Named::Unnamed, Some(machine)) => {
                let _ = writeln!(
                    text,
                    "  Nothing on this call named a machine, and {machine} is where it was placed."
                );
            }
            _ => {}
        }
        let said = |needle: &str| why.contains(needle);
        let bare = after_at(host);
        let lines: Option<Vec<String>> = if said("REMOTE HOST IDENTIFICATION HAS CHANGED") {
            Some(vec![
                "  Its host key is not the one recorded. The machine answered, so it is up;".into(),
                "  ssh will not talk to it until you say which key is right.".into(),
                "  If it was reinstalled or its key regenerated, that is expected:".into(),
                format!("    ssh-keygen -R {bare}"),
                "  then connect once by hand. If it was not, find out why the key changed".into(),
                "  before trusting it.".into(),
            ])
        } else if said("Host key verification failed") {
            Some(vec![
                "  Its host key is not in your known_hosts, and dibs connects with BatchMode,"
                    .into(),
                "  which cannot answer the question ssh is asking. The machine is almost".into(),
                "  certainly up: this is about trust, not reachability.".into(),
                format!("  Record the key by connecting once by hand:  ssh {host}"),
            ])
        } else if said("Permission denied") {
            let mut lines = vec![
                "  It answered and refused the login, so it is up and this is about keys.".into(),
            ];
            match self.accepted_key(&timeout, secs) {
                Some(key) => {
                    lines.push(format!(
                        "  It accepts your key {key}, which needs its passphrase, and no ssh agent holds it."
                    ));
                    lines.push(format!("  Load it once:  ssh-add {key}"));
                }
                None => lines.push(
                    "  It accepted none of your keys: check yours is in that account's authorized_keys."
                        .into(),
                ),
            }
            Some(lines)
        } else if said("Connection refused") {
            Some(vec![
                "  It answered and nothing is listening on the ssh port, so the machine is up"
                    .into(),
                "  and sshd is not.".into(),
            ])
        } else if said("Could not resolve hostname")
            || said("Name or service not known")
            || said("nodename nor servname")
        {
            Some(vec![
                format!("  The name '{bare}' does not resolve from here. A .local name needs mDNS"),
                "  and the same network; anything else needs DNS.".into(),
            ])
        } else if said("Network is unreachable") {
            Some(vec![
                "  This side has no route to it: the problem is your own network or VPN, not the"
                    .into(),
                "  machine. Nothing about it can be known from here until that is back.".into(),
            ])
        } else if said("No route to host") {
            Some(vec![
                "  Nothing answered at its address: it is asleep, off, or no longer at that address.".into(),
                "  A laptop asleep on its lid or battery looks exactly like this.".into(),
            ])
        } else {
            None
        };
        match lines {
            Some(lines) => lines.iter().for_each(|l| {
                let _ = writeln!(text, "{l}");
            }),
            None => {
                text.push_str(&self.tailscale());
                if let Some(first) = why.lines().next().filter(|_| !why.is_empty()) {
                    let _ = writeln!(text, "  ssh said: {first}");
                }
            }
        }
        text.push_str(
            "  Do not retry in a loop. Tell the user, and do the work that does not need the machine.\n",
        );
        text
    }

    /// ssh offers a key's public half before it needs the private one, so a machine that takes a
    /// key says so even when the key is locked.
    fn accepted_key(&self, timeout: &str, secs: u64) -> Option<String> {
        let verbose = bounded(
            Command::new("ssh").args([
                "-v",
                "-o",
                "BatchMode=yes",
                "-o",
                &format!("ConnectTimeout={timeout}"),
                &self.target.host,
                "true",
            ]),
            secs,
        );
        verbose.lines().find_map(|l| {
            let key = l.strip_prefix("debug1: Server accepts key: ")?;
            key.split(' ').next().map(str::to_string)
        })
    }

    /// Only for a machine tailscale carries: what it says about one reached over the LAN is not
    /// evidence either way.
    fn tailscale(&self) -> String {
        let Ok(out) = Command::new("tailscale")
            .arg("status")
            .stdin(Stdio::null())
            .output()
        else {
            return "  It did not answer: off, asleep, or not on this network.\n".into();
        };
        let state = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let target = &self.target.hostname;
        let wanted = target.to_lowercase();
        let peer = state.lines().find(|l| {
            let l = l.to_lowercase();
            l.match_indices(&wanted).any(|(at, _)| {
                let before = l[..at].chars().next_back();
                let after = l[at + wanted.len()..].chars().next();
                !wanted.is_empty()
                    && before.is_some_and(char::is_whitespace)
                    && after.is_some_and(char::is_whitespace)
            })
        });
        let has = |s: &str| state.contains(s);
        if has("Logged out") || has("logged out") || has("NeedsLogin") {
            "  Tailscale is logged out, which it is after a reboot.\n  Log in with: tailscale up\n"
                .into()
        } else if has("stopped") || has("Stopped") {
            "  Tailscale is stopped. Start it with: tailscale up\n".into()
        } else if let Some(peer) = peer.filter(|p| p.to_lowercase().contains("offline")) {
            format!("  Tailscale reports {target} offline: {peer}\n")
        } else if let Some(peer) = peer {
            format!("  Tailscale reports {target} up ({peer}), so this is an ssh problem.\n")
        } else {
            "  It did not answer: off, asleep, or not on this network. It is not\n  a tailnet peer either, so tailscale has nothing to say about it.\n".into()
        }
    }
}

/// What a command printed on both streams, given at most `secs` before it is stopped.
fn bounded(command: &mut Command, secs: u64) -> String {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Ok(child) = command.spawn() else {
        return String::new();
    };
    let pid = child.id();
    let (done, finished) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(secs)).is_err() {
            // SAFETY: the child is not reaped until wait_with_output returns, so the pid is its.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
    });
    let out = child.wait_with_output();
    let _ = done.send(());
    let _ = watchdog.join();
    out.map(|o| {
        let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&o.stderr));
        text.trim_end_matches('\n').to_string()
    })
    .unwrap_or_default()
}
