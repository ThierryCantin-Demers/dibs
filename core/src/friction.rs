//! One line about what got in the way, from the session that hit it.
//!
//! A missing recipe had a channel and nothing else did, so every other problem reached the user
//! by an agent choosing to mention it in chat. The same few gaps were found again session after
//! session, and the one report anybody wrote down lived in a scratchpad and went with it.

use crate::runs;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub struct Note {
    pub when: u64,
    pub text: String,
    pub by: String,
    pub version: String,
}

/// Beside the run records, and moved by a variable of its own: it answers for the same work, and
/// where it should live is the user's to decide, a repo they share with someone being one place
/// a complaint about their tooling does not belong.
pub fn path() -> Result<PathBuf, String> {
    match std::env::var_os("DIBS_FRICTION") {
        Some(p) => Ok(PathBuf::from(p)),
        None => {
            let home = std::env::var_os("HOME").ok_or("no HOME, and nowhere to record this")?;
            Ok(PathBuf::from(home).join(".local/state/dibs/friction.jsonl"))
        }
    }
}

/// Whitespace collapsed to one line, because what makes the list readable later is that each
/// report is one, and two sessions reporting the same thing have to land on the same text to be
/// counted as two.
pub fn note(text: &str, by: &str, version: &str, when: u64) -> Result<Note, String> {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("--friction takes one line: what got in the way, in your own words".into());
    }
    Ok(Note {
        when,
        text: text.chars().take(280).collect(),
        by: by.chars().take(48).collect(),
        version: version.chars().take(40).collect(),
    })
}

/// Appended, never rewritten, so two sessions reporting at the same moment cannot lose each
/// other's line.
pub fn append(path: &Path, n: &Note) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let line = serde_json::json!({
        "t": n.when,
        "text": n.text,
        "by": n.by,
        "dibs": n.version,
    });
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("{}: {e}", path.display()))
}

pub fn load(path: &Path) -> Vec<Note> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            let text = s("text");
            (!text.is_empty()).then(|| Note {
                when: v.get("t").and_then(Value::as_u64).unwrap_or_default(),
                text,
                by: s("by"),
                version: s("dibs"),
            })
        })
        .collect()
}

/// Case and a trailing stop are not a different report, and reading the same complaint as two
/// hides the thing this exists to show: that it keeps happening.
fn same(text: &str) -> String {
    text.to_lowercase().trim_end_matches(['.', '!']).to_string()
}

/// One report is a nuisance somebody worked around. The same one three times is the
/// specification for a fix, so the count leads and the recurring ones sort to the top.
pub fn report(notes: &[Note]) -> String {
    if notes.is_empty() {
        return "\nNothing has been reported as friction. An agent that had to work around dibs\n\
                records it with: dibs --friction '<one line>'\n"
            .to_string();
    }
    let mut seen: BTreeMap<String, (usize, &Note, &Note)> = BTreeMap::new();
    for n in notes {
        seen.entry(same(&n.text))
            .and_modify(|e| {
                e.0 += 1;
                if n.when >= e.2.when {
                    e.2 = n;
                }
                if n.when < e.1.when {
                    e.1 = n;
                }
            })
            .or_insert((1, n, n));
    }
    let mut ordered: Vec<(usize, &Note, &Note)> = seen.into_values().collect();
    ordered.sort_by(|a, b| b.0.cmp(&a.0).then(b.2.when.cmp(&a.2.when)));

    let mut out = String::from("\nWhat got in the way:\n\n");
    for (n, first, last) in &ordered {
        out.push_str(&format!("  {n:>3}x  {}\n", last.text));
        let by = if last.by.is_empty() { String::new() } else { format!(" by {}", last.by) };
        let at = if last.version.is_empty() { String::new() } else { format!(", dibs {}", last.version) };
        let (was, now) = (runs::date(first.when as i64), runs::date(last.when as i64));
        if *n > 1 && was != now {
            out.push_str(&format!("       first {was}, last {now}{by}{at}\n"));
        } else {
            out.push_str(&format!("       {now}{by}{at}\n"));
        }
    }
    let repeated = ordered.iter().filter(|(n, _, _)| *n > 1).count();
    match repeated {
        0 => {}
        1 => out.push_str("\nOne of these has been hit more than once, which is where a fix pays.\n"),
        n => out.push_str(&format!(
            "\n{n} of these have been hit more than once, which is where a fix pays.\n"
        )),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_is_one_line_however_it_was_typed() {
        let n = note("  the trailer says built=nothing\n  but it did build  ", "a", "abc", 10).unwrap();
        assert_eq!(n.text, "the trailer says built=nothing but it did build");
        assert!(note("   ", "a", "abc", 10).is_err());
    }

    #[test]
    fn the_same_thing_said_twice_is_counted_as_twice() {
        let notes = vec![
            note("--stream does nothing in a batch", "one", "aaa", 100).unwrap(),
            note("Nothing else", "two", "aaa", 200).unwrap(),
            note("--stream does nothing in a batch.", "three", "bbb", 300).unwrap(),
        ];
        let out = report(&notes);
        assert!(out.contains("    2x  --stream does nothing in a batch."), "{out}");
        assert!(out.contains("by three, dibs bbb"), "the session to ask is the last one: {out}");
        assert!(out.contains("first 1970-01-01 00:01, last 1970-01-01 00:05"), "{out}");
        assert!(out.contains("    1x  Nothing else"), "{out}");
        assert!(out.find("2x").unwrap() < out.find("1x").unwrap(), "recurring first: {out}");
    }

    #[test]
    fn a_line_survives_the_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("dibs-friction-{}", std::process::id()));
        let path = dir.join("friction.jsonl");
        let _ = std::fs::remove_dir_all(&dir);
        let quoted = note(r#"a "quoted" \ backslash, and a tab	here"#, "me", "abc", 7).unwrap();
        append(&path, &quoted).unwrap();
        let back = load(&path);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].text, r#"a "quoted" \ backslash, and a tab here"#);
        assert_eq!(back[0].by, "me");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
