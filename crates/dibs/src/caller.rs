//! Who is calling: the session a job belongs to, and the name a person knows it by.

use dibs_format::FileName;
use dibs_runner::short_hostname;
use std::{ffi::OsString, path::PathBuf, process::Stdio, sync::mpsc, thread, time::Duration};

/// The longest name a record carries.
const NAME_CHARS: usize = 48;
/// Every dibs call names its caller, so a herdr that does not answer by then is passed over.
const HERDR_WITHIN: Duration = Duration::from_millis(500);

/// Ownership keys on the session id, which never moves; only the display uses the title.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caller {
    pub id: String,
    pub name: String,
}

impl Caller {
    pub fn from_env() -> Caller {
        Caller::from_vars(|k| std::env::var_os(k), Caller::herdr_name)
    }

    /// `herdr` answers for the pane named by `HERDR_PANE_ID`, so a rename shows from the next call.
    pub fn from_vars(
        read: impl Fn(&str) -> Option<OsString>,
        herdr: impl Fn(&str) -> Option<String>,
    ) -> Caller {
        let var = |k: &str| {
            read(k)
                .map(|v| v.to_string_lossy().into_owned())
                .filter(|v| !v.is_empty())
        };
        let session = var("CLAUDE_CODE_HOST_SESSION_ID").or_else(|| var("CLAUDE_CODE_SESSION_ID"));
        let user = var("USER").unwrap_or_else(|| "someone".into());
        let agent = var("DIBS_AGENT");
        let id = match (&session, &agent) {
            (Some(session), _) => session.clone(),
            (None, Some(agent)) => format!(
                "agent-{user}@{}-{}",
                short_hostname(var("HOSTNAME")),
                cksum(agent.as_bytes())
            ),
            (None, None) => format!("shell-{user}@{}", short_hostname(var("HOSTNAME"))),
        };
        let named = agent.or_else(|| var("HERDR_PANE_ID").and_then(|pane| herdr(&pane)));
        let name = match (named, session) {
            (Some(name), _) => name,
            (None, None) if var("CODEX_SHELL").as_deref() == Some("1") => "a Codex session".into(),
            (None, None) => format!("{user} at a shell"),
            (None, Some(session)) => var("HOME")
                .and_then(|home| Caller::title(&PathBuf::from(home), &session))
                .unwrap_or_else(|| {
                    format!(
                        "session {}",
                        session.strip_prefix("local_").unwrap_or(&session)
                    )
                }),
        };
        Caller {
            id,
            name: name.chars().take(NAME_CHARS).collect(),
        }
    }

    /// The name herdr gives the agent in a pane; None when herdr is missing, fails or is slow.
    fn herdr_name(pane: &str) -> Option<String> {
        let child = std::process::Command::new("herdr")
            .args(["agent", "get", pane])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let pid = child.id();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(child.wait_with_output()));
        let Ok(Ok(out)) = rx.recv_timeout(HERDR_WITHIN) else {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .stderr(Stdio::null())
                .status();
            return None;
        };
        if !out.status.success() {
            return None;
        }
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let name = doc.pointer("/result/agent/name")?.as_str()?;
        (!name.is_empty()).then(|| name.to_string())
    }

    /// The title the desktop keeps for a session, in a file named after its id.
    fn title(home: &std::path::Path, session: &str) -> Option<String> {
        let sessions = home.join(".config/Claude/claude-code-sessions");
        let file = format!("{session}.json");
        let outer = std::fs::read_dir(sessions).ok()?;
        let mut candidates: Vec<PathBuf> = outer
            .flatten()
            .filter_map(|a| std::fs::read_dir(a.path()).ok())
            .flat_map(|inner| inner.flatten().map(|b| b.path().join(&file)))
            .collect();
        candidates.sort();
        candidates.iter().find_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
            let title = match doc.get("title")? {
                serde_json::Value::Null | serde_json::Value::Bool(false) => return None,
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (!title.is_empty()).then_some(title)
        })
    }

    /// The id as a file name.
    pub fn file_name(&self) -> String {
        FileName::of(&self.id).into()
    }
}

/// POSIX `cksum`: a CRC-32 over the bytes and then their length.
fn cksum(bytes: &[u8]) -> u32 {
    let step = |mut crc: u32, byte: u8| {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = match crc & 0x8000_0000 {
                0 => crc << 1,
                _ => (crc << 1) ^ 0x04C1_1DB7,
            };
        }
        crc
    };
    let mut crc = bytes.iter().fold(0, |crc, b| step(crc, *b));
    let mut length = bytes.len();
    while length > 0 {
        crc = step(crc, (length & 0xff) as u8);
        length >>= 8;
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn caller(vars: &[(&str, &str)]) -> Caller {
        let vars: BTreeMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Caller::from_vars(
            |k| vars.get(k).map(OsString::from),
            |pane| (pane == "w1:p2").then(|| "orion".to_string()),
        )
    }

    #[test]
    fn cksum_matches_the_posix_tool() {
        assert_eq!(cksum(b"abc"), 1219131554);
        assert_eq!(cksum(b""), 4294967295);
    }

    #[test]
    fn a_session_is_its_id_and_a_shell_is_named_for_what_it_is() {
        let session = caller(&[
            ("CLAUDE_CODE_SESSION_ID", "local_abc"),
            ("HOME", "/nowhere"),
        ]);
        assert_eq!(session.id, "local_abc");
        assert_eq!(session.name, "session abc");
        let shell = caller(&[("USER", "me"), ("HOSTNAME", "box.lan")]);
        assert_eq!(shell.id, "shell-me@box");
        assert_eq!(shell.name, "me at a shell");
        let agent = caller(&[("USER", "me"), ("HOSTNAME", "box"), ("DIBS_AGENT", "abc")]);
        assert_eq!(agent.id, "agent-me@box-1219131554");
        assert_eq!(agent.name, "abc");
    }

    #[test]
    fn a_herdr_pane_names_a_session_unless_dibs_agent_does() {
        let session = [("CLAUDE_CODE_SESSION_ID", "abc"), ("HOME", "/nowhere")];
        let in_herdr = caller(&[session[0], session[1], ("HERDR_PANE_ID", "w1:p2")]);
        assert_eq!(in_herdr.id, "abc");
        assert_eq!(in_herdr.name, "orion");
        let chosen = caller(&[
            session[0],
            session[1],
            ("HERDR_PANE_ID", "w1:p2"),
            ("DIBS_AGENT", "reduce sweep"),
        ]);
        assert_eq!(chosen.name, "reduce sweep");
        let unknown_pane = caller(&[session[0], session[1], ("HERDR_PANE_ID", "w9:p9")]);
        assert_eq!(unknown_pane.name, "session abc");
    }
}
