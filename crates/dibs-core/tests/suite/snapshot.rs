//! Outputs pinned word for word; `UPDATE_SNAPSHOTS=<name>,<name>` or `=all` accepts a change.

use crate::harness::{CORE, DIBS, Output, Sandbox, hostname, repo_root};
use regex::Regex;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const SNAPSHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/suite/snapshots");
const CONTEXT: usize = 3;

fn snapshot_path(name: &str) -> PathBuf {
    Path::new(SNAPSHOTS).join(format!("{name}.txt"))
}

/// Whether `UPDATE_SNAPSHOTS` names this snapshot, or says `all`.
fn accepting(name: &str) -> bool {
    std::env::var("UPDATE_SNAPSHOTS")
        .is_ok_and(|v| v.split(',').any(|n| n.trim() == name || n.trim() == "all"))
}

/// Fails unless `text` is the snapshot called `name`.
pub fn snapshot(name: &str, text: &str) {
    let path = snapshot_path(name);
    let want = fs::read_to_string(&path).ok();
    if want.as_deref() == Some(text) {
        return;
    }
    if accepting(name) {
        fs::create_dir_all(SNAPSHOTS).unwrap();
        fs::write(&path, text).unwrap();
        return;
    }
    match want {
        None => panic!(
            "no snapshot {}; UPDATE_SNAPSHOTS={name} cargo test writes it from this:\n{text}",
            path.display()
        ),
        Some(want) => panic!(
            "{name} differs from its snapshot, - as kept and + as printed now. UPDATE_SNAPSHOTS={name} cargo test accepts it.\n{}",
            diff(&want, text)
        ),
    }
}

/// The lines that differ, with a few around each, from the longest run of lines both share.
fn diff(want: &str, got: &str) -> String {
    let (a, b): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
    let mut common = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            common[i][j] = if a[i] == b[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let mut lines: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            lines.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || common[i][j + 1] >= common[i + 1][j]) {
            lines.push(('+', b[j]));
            j += 1;
        } else {
            lines.push(('-', a[i]));
            i += 1;
        }
    }
    let near = |k: usize| {
        lines[k.saturating_sub(CONTEXT)..(k + CONTEXT + 1).min(lines.len())]
            .iter()
            .any(|(m, _)| *m != ' ')
    };
    let mut out = String::new();
    let mut skipped = false;
    for (k, (mark, line)) in lines.iter().enumerate() {
        if near(k) {
            if skipped {
                out.push_str("  ...\n");
            }
            let _ = writeln!(out, "{mark} {line}");
            skipped = false;
        } else {
            skipped = true;
        }
    }
    if want.ends_with('\n') != got.ends_with('\n') {
        out.push_str("  (and the two differ in whether they end with a newline)\n");
    }
    out
}

/// Text with what differs from run to run replaced by a name for it: the sandbox's paths, this
/// computer's name and user, job and batch ids, and wall-clock times. A test adds rules for the
/// pids and durations in what it reads, since only it knows which numbers are measured.
#[derive(Clone)]
pub struct Normal {
    rules: Vec<(Regex, String)>,
}

impl Normal {
    pub fn of(s: &Sandbox) -> Normal {
        Normal::paths(s)
            .rule(r"\b[0-9]{14}-[0-9]+\b", "<job>")
            .rule(r"\b[0-9]{8}-[0-9]{6}-[0-9]+\b", "<batch>")
            .rule(r"\b[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}([+-][0-9]{2}:?[0-9]{2}|Z)?", "<when>")
    }

    /// Only what places the text on this computer: paths, its name and its user.
    pub fn paths(s: &Sandbox) -> Normal {
        let mut n = Normal { rules: Vec::new() };
        for (path, name) in [(DIBS, "<dibs>"), (CORE, "<dibs-core>")] {
            if let Ok(real) = Path::new(path).canonicalize() {
                n = n.literal(&real.display().to_string(), name);
            }
            n = n.literal(path, name);
        }
        if let Ok(real) = s.root.canonicalize()
            && real != s.root
        {
            n = n.literal(&real.display().to_string(), "<root>");
        }
        n = n
            .literal(&s.root.display().to_string(), "<root>")
            .literal(&repo_root().display().to_string(), "<repo>");
        if let Some(name) = s.root.file_name() {
            n = n.literal(&name.to_string_lossy(), "<root-name>");
        }
        n = n.word(hostname(), "<host>");
        if let Some(user) = std::env::var("USER").ok().filter(|u| u.len() > 1) {
            n = n.word(&user, "<user>");
        }
        n
    }

    /// Every match of `re`, with `$1` and the like in `with` naming its groups.
    pub fn rule(mut self, re: &str, with: &str) -> Normal {
        self.rules.push((
            Regex::new(re).unwrap_or_else(|e| panic!("{re}: {e}")),
            with.to_string(),
        ));
        self
    }

    pub fn literal(self, text: &str, with: &str) -> Normal {
        if text.is_empty() {
            return self;
        }
        let re = regex::escape(text);
        self.rule(&re, &with.replace('$', "$$"))
    }

    /// `text` only where it stands as a word of its own.
    pub fn word(self, text: &str, with: &str) -> Normal {
        if text.is_empty() {
            return self;
        }
        let re = format!(
            r"(^|[^A-Za-z0-9_-]){}($|[^A-Za-z0-9_-])",
            regex::escape(text)
        );
        self.rule(&re, &format!("${{1}}{}${{2}}", with.replace('$', "$$")))
    }

    /// The durations a job's own clock decides, which no fixture can hold still.
    pub fn clocked(self) -> Normal {
        self.rule(r"queued [0-9]+s  ran [0-9]+s", "queued <s>  ran <s>")
            .rule(r"(ready after) [0-9]+s", "$1 <s>")
            .rule(r"(exit [0-9]+), [0-9hms]+ ago\.", "$1, <dur> ago.")
            .rule(r"(acquired the [a-z]+ lock after) [0-9hms]+", "$1 <dur>")
            .rule(r"(that --peek took) [0-9hms]+", "$1 <dur>")
            .rule(
                r"(held|It has been running|holding the lock for) [0-9]+[hms][0-9hms]*",
                "$1 <dur>",
            )
    }

    /// `pid N`, `--kill N` and the other places a running process is named.
    pub fn pids(self) -> Normal {
        self.rule(r"\bpid ?[0-9]+\b", "pid <pid>")
            .rule(
                r"(--kill|--out|kill -9|SIGTERM to|SIGKILL to) [0-9]+\b",
                "$1 <pid>",
            )
            .rule(
                r"\b(holder|waiting|cpu|batch|hold|with|work)\.[0-9]+\b",
                "$1.<pid>",
            )
    }

    pub fn apply(&self, text: &str) -> String {
        let mut text = text.to_string();
        for (re, with) in &self.rules {
            // A word rule consumes the character on each side of a match, so two matches that
            // share one are replaced in a second pass.
            for _ in 0..2 {
                text = re.replace_all(&text, with.as_str()).into_owned();
            }
        }
        text
    }

    /// A call's exit and both its streams, each line marked with the stream it came from: `1|`
    /// for stdout and `2|` for stderr. Their interleaving is lost, as in `Output::all`.
    pub fn output(&self, out: &Output) -> String {
        shown(out.code, &self.apply(&out.stdout), &self.apply(&out.stderr))
    }
}

fn shown(code: i32, stdout: &str, stderr: &str) -> String {
    let mut s = format!("-> exit {code}\n");
    for (mark, text) in [("1|", stdout), ("2|", stderr)] {
        for line in text.lines() {
            let _ = writeln!(s, "{mark} {line}");
        }
        if !text.is_empty() && !text.ends_with('\n') {
            let _ = writeln!(s, "{mark} (no newline at the end)");
        }
    }
    s
}

/// Arguments as a shell would need them typed, for a section's title.
pub fn typed(args: &[&str]) -> String {
    let plain = |a: &str| {
        !a.is_empty()
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,%+".contains(c))
    };
    let doubled = |a: &str| !a.contains(['"', '$', '`', '\\', '!']);
    args.iter()
        .map(|a| match (plain(a), a.contains('\''), doubled(a)) {
            (true, ..) => a.to_string(),
            (false, false, _) => format!("'{a}'"),
            (false, true, true) => format!("\"{a}\""),
            (false, true, false) => format!("'{}'", a.replace('\'', r"'\''")),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A one-line JSON document laid out one value per line, keys in the order they were written,
/// since the order is part of what a reader of the raw line sees.
pub fn json_lines(doc: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    let mut chars = doc.trim_end().chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                if matches!(chars.peek(), Some('}' | ']')) {
                    out.push(chars.next().unwrap());
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c => out.push(c),
        }
    }
    out.push('\n');
    out
}

/// Sections of one snapshot, each under the command that printed it.
#[derive(Default)]
pub struct Transcript {
    text: String,
}

impl Transcript {
    pub fn section(&mut self, title: &str, body: &str) {
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        let _ = writeln!(self.text, "== {title}");
        self.text.push_str(body);
        if !body.is_empty() && !body.ends_with('\n') {
            self.text.push('\n');
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}
