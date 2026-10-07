//! One line about what got in the way, from the session that hit it.
//!
//! A missing recipe had a channel and nothing else did, so every other problem reached the user
//! by an agent choosing to mention it in chat. The same few gaps were found again session after
//! session, and the one report anybody wrote down lived in a scratchpad and went with it.

use crate::{
    paths::{FileError, Paths},
    records::error::{Ledger, RecordsError},
};
use dibs_format::{FrictionNote, Moment};
use std::{collections::BTreeMap, fmt, io::Write as _, path::PathBuf};

/// The friction notes this computer kept, one line each.
pub struct FrictionLog {
    path: PathBuf,
}

impl FrictionLog {
    /// Beside the run records, and moved by a variable of its own: it answers for the same work,
    /// and where it should live is the user's to decide. Sharing it is theirs to choose too, with
    /// DIBS_REPORTS, and never a default.
    pub fn here() -> Result<FrictionLog, RecordsError> {
        let path = Paths::from_env()
            .friction()
            .ok_or(RecordsError::NoHome(Ledger::Friction))?;
        Ok(FrictionLog { path })
    }

    /// Appended, never rewritten, so two sessions reporting at the same moment cannot lose each
    /// other's line.
    pub fn append(&self, note: &FrictionNote) -> Result<(), RecordsError> {
        let path = &self.path;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(FileError::at(dir))?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(FileError::at(path))?;
        Ok(writeln!(f, "{}", note.to_line()).map_err(FileError::at(path))?)
    }

    pub fn notes(&self) -> Vec<FrictionNote> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        text.lines()
            .filter_map(|l| l.parse::<FrictionNote>().ok())
            .filter(|n| !n.text.is_empty())
            .collect()
    }
}

/// One thing that got in the way, however many times it was reported.
struct Complaint<'a> {
    times: usize,
    first: &'a FrictionNote,
    last: &'a FrictionNote,
}

impl Complaint<'_> {
    /// Case and a trailing stop are not a different report, and reading the same complaint as
    /// two hides the thing this exists to show: that it keeps happening.
    fn key(text: &str) -> String {
        text.to_lowercase().trim_end_matches(['.', '!']).to_string()
    }
}

/// What got in the way, as `dibs gaps` prints it.
pub struct Complaints<'a> {
    recurring_first: Vec<Complaint<'a>>,
}

impl<'a> Complaints<'a> {
    /// One report is a nuisance somebody worked around. The same one three times is the
    /// specification for a fix, so the count leads and the recurring ones sort to the top.
    pub fn of(notes: &'a [FrictionNote]) -> Complaints<'a> {
        let mut seen: BTreeMap<String, Complaint<'a>> = BTreeMap::new();
        for n in notes {
            seen.entry(Complaint::key(&n.text))
                .and_modify(|c| {
                    c.times += 1;
                    if n.when >= c.last.when {
                        c.last = n;
                    }
                    if n.when < c.first.when {
                        c.first = n;
                    }
                })
                .or_insert(Complaint {
                    times: 1,
                    first: n,
                    last: n,
                });
        }
        let mut recurring_first: Vec<Complaint<'a>> = seen.into_values().collect();
        recurring_first.sort_by(|a, b| b.times.cmp(&a.times).then(b.last.when.cmp(&a.last.when)));
        Complaints { recurring_first }
    }
}

impl fmt::Display for Complaints<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.recurring_first.is_empty() {
            return f.write_str(
                "\nNothing has been reported as friction. An agent that had to work around dibs\n\
                 records it with: dibs --friction '<one line>'\n",
            );
        }
        f.write_str("\nWhat got in the way:\n\n")?;
        for Complaint { times, first, last } in &self.recurring_first {
            writeln!(f, "  {times:>3}x  {}", last.text)?;
            let by = if last.by.is_empty() {
                String::new()
            } else {
                format!(" by {}", last.by)
            };
            let at = if last.version.is_empty() {
                String::new()
            } else {
                format!(", dibs {}", last.version)
            };
            let was = Moment::in_zone(first.when, 0).minute();
            let now = Moment::in_zone(last.when, 0).minute();
            if *times > 1 && was != now {
                writeln!(f, "       first {was}, last {now}{by}{at}")?;
            } else {
                writeln!(f, "       {now}{by}{at}")?;
            }
        }
        let repeated = self.recurring_first.iter().filter(|c| c.times > 1).count();
        match repeated {
            0 => Ok(()),
            1 => f.write_str(
                "\nOne of these has been hit more than once, which is where a fix pays.\n",
            ),
            n => write!(
                f,
                "\n{n} of these have been hit more than once, which is where a fix pays.\n"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(text: &str, by: &str, version: &str, when: u64) -> FrictionNote {
        FrictionNote::new(text, by, version, when).unwrap()
    }

    #[test]
    fn the_same_thing_said_twice_is_counted_as_twice() {
        let notes = vec![
            note("--stream does nothing in a batch", "one", "aaa", 100),
            note("Nothing else", "two", "aaa", 200),
            note("--stream does nothing in a batch.", "three", "bbb", 300),
        ];
        let out = Complaints::of(&notes).to_string();
        assert!(
            out.contains("    2x  --stream does nothing in a batch."),
            "{out}"
        );
        assert!(
            out.contains("by three, dibs bbb"),
            "the session to ask is the last one: {out}"
        );
        assert!(
            out.contains("first 1970-01-01 00:01, last 1970-01-01 00:05"),
            "{out}"
        );
        assert!(out.contains("    1x  Nothing else"), "{out}");
        assert!(
            out.find("2x").unwrap() < out.find("1x").unwrap(),
            "recurring first: {out}"
        );
    }

    #[test]
    fn a_line_survives_the_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("dibs-friction-{}", std::process::id()));
        let log = FrictionLog {
            path: dir.join("friction.jsonl"),
        };
        let _ = std::fs::remove_dir_all(&dir);
        let quoted = note(r#"a "quoted" \ backslash, and a tab	here"#, "me", "abc", 7);
        log.append(&quoted).unwrap();
        let back = log.notes();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].text, r#"a "quoted" \ backslash, and a tab here"#);
        assert_eq!(back[0].by, "me");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
