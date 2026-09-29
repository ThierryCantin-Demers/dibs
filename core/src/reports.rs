//! Friction filed where the people who fix dibs see it: an issue in a private repo the team
//! shares, with the answers brought back to the session that reported it.

use crate::friction::Note;
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

pub const LABEL: &str = "dibs-friction";
const FORWARD_READY: Duration = Duration::from_secs(30);
const LISTED: &str = "200";

/// Opt-in per person, since a report says what they were doing: without it, friction stays on
/// the machine it was reported on.
pub fn repo() -> Option<String> {
    std::env::var("DIBS_REPORTS").ok().filter(|r| is_repo(r))
}

fn is_repo(r: &str) -> bool {
    matches!(r.split_once('/'), Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/'))
}

fn gh(args: &[&str]) -> Result<String, String> {
    let out = Command::new("gh")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run gh: {e}"))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

fn host() -> String {
    Command::new("hostname")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn number(url: &str) -> Option<u64> {
    url.trim().rsplit('/').next()?.parse().ok()
}

/// A public repo is refused: a report carries the paths, repos and commands of whoever filed it.
pub fn file(repo: &str, n: &Note) -> Result<u64, String> {
    let visibility = gh(&["repo", "view", repo, "--json", "visibility", "--jq", ".visibility"])?;
    if visibility == "PUBLIC" {
        return Err(format!("{repo} is public, and a report carries the paths and commands of whoever filed it"));
    }
    let title: String = n.text.chars().take(100).collect();
    let body = format!(
        "{}\n\n| | |\n|---|---|\n| reported by | {} |\n| dibs | {} |\n| from | {} |\n",
        n.text,
        n.by,
        n.version,
        host()
    );
    let create = ["issue", "create", "-R", repo, "--title", &title, "--body", &body, "--label", LABEL];
    let url = match gh(&create) {
        Err(e) if e.contains(LABEL) => {
            gh(&["label", "create", LABEL, "-R", repo, "--color", "d4c5f9", "--description", "filed by dibs --friction"])?;
            gh(&create)?
        }
        other => other?,
    };
    number(&url).ok_or_else(|| format!("gh printed no issue: {url}"))
}

/// What has already been said or acted on, one key a line: `#<n>` for a report, a comment's URL
/// for a comment.
struct Keys {
    path: PathBuf,
    set: BTreeSet<String>,
    fresh: bool,
}

impl Keys {
    fn load(name: &str) -> Result<Keys, String> {
        let dir = match std::env::var_os("XDG_STATE_HOME") {
            Some(d) => PathBuf::from(d),
            None => PathBuf::from(std::env::var_os("HOME").ok_or("no HOME to keep reports' state in")?).join(".local/state"),
        };
        let path = dir.join("dibs").join(name);
        let text = std::fs::read_to_string(&path);
        let fresh = text.is_err();
        let set = text.unwrap_or_default().lines().map(str::to_string).collect();
        Ok(Keys { path, set, fresh })
    }

    fn insert(&mut self, key: &str) -> bool {
        self.set.insert(key.to_string())
    }

    fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text: String = self.set.iter().map(|k| format!("{k}\n")).collect();
        std::fs::write(&self.path, text).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

fn first_line(body: &Value) -> String {
    let line = body.as_str().unwrap_or_default().lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
    line.chars().take(200).collect()
}

fn login(v: &Value) -> &str {
    v["login"].as_str().unwrap_or("someone")
}

fn listed(repo: &str) -> Result<Vec<Value>, String> {
    let text = gh(&["issue", "list", "-R", repo, "--label", LABEL, "--state", "all", "--limit", LISTED, "--json", "number,title,url,state,author,comments"])?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("gh issue list printed something unreadable: {e}"))?;
    Ok(v.as_array().cloned().unwrap_or_default())
}

/// Answers to the reports this session filed, each said once.
pub fn replies(repo: &str, notes: &[Note], by: &str) -> Result<Vec<String>, String> {
    let filed: BTreeSet<u64> = notes.iter().filter(|n| n.by == by).filter_map(|n| n.issue).collect();
    if filed.is_empty() {
        return Ok(Vec::new());
    }
    let mut shown = Keys::load("reports-shown")?;
    let mut out = Vec::new();
    for issue in listed(repo)? {
        let Some(n) = issue["number"].as_u64().filter(|n| filed.contains(n)) else { continue };
        for c in issue["comments"].as_array().into_iter().flatten() {
            let url = c["url"].as_str().unwrap_or_default();
            if url.is_empty() || !shown.insert(url) {
                continue;
            }
            out.push(format!("dibs: your report #{n} was answered by {}: {}\n  {url}", login(&c["author"]), first_line(&c["body"])));
        }
    }
    shown.save()?;
    Ok(out)
}

/// Everything not yet woken on. The first time, that is the open reports, and the rest is taken
/// as read rather than replayed.
fn catch_up(repo: &str, woken: &mut Keys, mine: &Keys) -> Result<Vec<String>, String> {
    let first = woken.fresh;
    let mut out = Vec::new();
    for issue in listed(repo)? {
        let n = issue["number"].as_u64().unwrap_or_default();
        let open = issue["state"].as_str() == Some("OPEN");
        if woken.insert(&format!("#{n}")) && (!first || open) {
            out.push(format!(
                "report #{n} from {}: {}\n  {}",
                login(&issue["author"]),
                issue["title"].as_str().unwrap_or_default(),
                issue["url"].as_str().unwrap_or_default()
            ));
        }
        for c in issue["comments"].as_array().into_iter().flatten() {
            let url = c["url"].as_str().unwrap_or_default();
            if url.is_empty() || mine.set.contains(url) || !woken.insert(url) || first {
                continue;
            }
            out.push(format!("comment on #{n} from {}: {}\n  {url}", login(&c["author"]), first_line(&c["body"])));
        }
    }
    woken.fresh = false;
    Ok(out)
}

fn labelled(issue: &Value) -> bool {
    issue["labels"].as_array().into_iter().flatten().any(|l| l["name"].as_str() == Some(LABEL))
}

fn on_event(event: &str, body: &Value, woken: &mut Keys, mine: &Keys) -> Vec<String> {
    let issue = &body["issue"];
    if !labelled(issue) {
        return Vec::new();
    }
    let n = issue["number"].as_u64().unwrap_or_default();
    match (event, body["action"].as_str().unwrap_or_default()) {
        ("issues", "opened" | "reopened") if woken.insert(&format!("#{n}")) => vec![format!(
            "report #{n} from {}: {}\n  {}",
            login(&issue["user"]),
            issue["title"].as_str().unwrap_or_default(),
            issue["html_url"].as_str().unwrap_or_default()
        )],
        ("issue_comment", "created") => {
            let c = &body["comment"];
            let url = c["html_url"].as_str().unwrap_or_default();
            if url.is_empty() || mine.set.contains(url) || !woken.insert(url) {
                return Vec::new();
            }
            vec![format!("comment on #{n} from {}: {}\n  {url}", login(&c["user"]), first_line(&c["body"]))]
        }
        _ => Vec::new(),
    }
}

/// One webhook delivery: its event name and payload, answered so the forwarder moves on.
fn delivery(stream: TcpStream) -> Option<(String, Value)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let (mut event, mut length) = (String::new(), 0usize);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "x-github-event" => event = v.trim().to_string(),
                "content-length" => length = v.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    let _ = (&stream).write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    Some((event, serde_json::from_slice(&body).ok()?))
}

/// `gh webhook forward`, which relays the repo's webhooks over its own connection to GitHub, so
/// nothing here has to be reachable from outside. The webhook it made goes when it does.
struct Forward(Child);

impl Forward {
    fn start(repo: &str, port: u16) -> Result<Forward, String> {
        let mut child = Command::new("gh")
            .args(["webhook", "forward", "--repo", repo, "--events", "issues,issue_comment", "--url", &format!("http://127.0.0.1:{port}/")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run gh webhook forward: {e}"))?;
        let stderr = child.stderr.take().ok_or("gh webhook forward has no stderr")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut said = String::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.contains("Forwarding") {
                    let _ = tx.send(Ok(()));
                }
                said.push_str(&line);
                said.push('\n');
            }
            let _ = tx.send(Err(said));
        });
        let mut forward = Forward(child);
        match rx.recv_timeout(FORWARD_READY) {
            Ok(Ok(())) => Ok(forward),
            Ok(Err(said)) => Err(format!(
                "gh webhook forward stopped before it was listening. It needs the cli/gh-webhook extension, and \
                 admin on {repo}; only one forward per repo can run at a time.\n{}",
                said.trim()
            )),
            Err(_) => {
                let _ = forward.0.kill();
                Err(format!("gh webhook forward did not start listening within {}s", FORWARD_READY.as_secs()))
            }
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Returns once a report, or an answer that was not posted from here, lands. What landed while
/// nothing listened is read first, since GitHub delivers a webhook once or not at all.
pub fn wait(repo: &str) -> Result<Vec<String>, String> {
    let mut woken = Keys::load("reports-woken")?;
    let mine = Keys::load("reports-mine")?;
    let news = catch_up(repo, &mut woken, &mine)?;
    if !news.is_empty() {
        woken.save()?;
        return Ok(news);
    }
    woken.save()?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("no local port to listen on: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let _forward = Forward::start(repo, port)?;
    let news = catch_up(repo, &mut woken, &mine)?;
    if !news.is_empty() {
        woken.save()?;
        return Ok(news);
    }
    for stream in listener.incoming() {
        let Some((event, body)) = stream.ok().and_then(delivery) else { continue };
        let news = on_event(&event, &body, &mut woken, &mine);
        if !news.is_empty() {
            woken.save()?;
            return Ok(news);
        }
    }
    Err("stopped listening for reports".into())
}

/// Posted from here, so a wait does not wake on its own answer.
pub fn reply(repo: &str, issue: u64, text: &str, close: bool) -> Result<String, String> {
    let n = issue.to_string();
    let url = gh(&["issue", "comment", &n, "-R", repo, "--body", text])?;
    let mut mine = Keys::load("reports-mine")?;
    mine.insert(url.trim());
    mine.save()?;
    if close {
        gh(&["issue", "close", &n, "-R", repo])?;
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Keys {
        Keys { path: PathBuf::new(), set: BTreeSet::new(), fresh: false }
    }

    #[test]
    fn a_comment_wakes_once_and_never_on_an_answer_posted_from_here() {
        let url = "https://github.com/o/r/issues/3#issuecomment-9";
        let body = |u: &str| serde_json::json!({
            "action": "created",
            "issue": { "number": 3, "labels": [{ "name": LABEL }] },
            "comment": { "html_url": u, "user": { "login": "sam" }, "body": "still broken\nmore" },
        });
        let (mut woken, mut mine) = (keys(), keys());
        assert_eq!(on_event("issue_comment", &body(url), &mut woken, &mine), vec![format!("comment on #3 from sam: still broken\n  {url}")]);
        assert!(on_event("issue_comment", &body(url), &mut woken, &mine).is_empty(), "once");
        mine.insert("https://github.com/o/r/issues/3#issuecomment-10");
        assert!(on_event("issue_comment", &body("https://github.com/o/r/issues/3#issuecomment-10"), &mut woken, &mine).is_empty(), "not its own");
    }

    #[test]
    fn only_a_labelled_issue_opening_is_a_report() {
        let opened = |labels: Value, action: &str| serde_json::json!({
            "action": action,
            "issue": { "number": 5, "labels": labels, "title": "t", "html_url": "u", "user": { "login": "sam" } },
        });
        let (mut woken, mine) = (keys(), keys());
        assert!(on_event("issues", &opened(serde_json::json!([]), "opened"), &mut woken, &mine).is_empty(), "unlabelled");
        assert!(on_event("issues", &opened(serde_json::json!([{ "name": LABEL }]), "closed"), &mut woken, &mine).is_empty(), "closing");
        assert_eq!(on_event("issues", &opened(serde_json::json!([{ "name": LABEL }]), "opened"), &mut woken, &mine).len(), 1);
    }

    #[test]
    fn a_repo_is_owner_and_name() {
        for (v, ok) in [("o/r", true), ("r", false), ("o/r/x", false), ("/r", false), ("o/", false)] {
            assert_eq!(is_repo(v), ok, "{v}");
        }
    }
}
