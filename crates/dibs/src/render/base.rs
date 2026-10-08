use crate::machine::Answer;
use dibs_format::MachineName;
use std::{fmt, path::Path};

/// What one machine said when asked among several.
#[derive(Debug)]
pub struct Answered {
    pub machine: MachineName,
    pub answer: Answer,
}

/// Each machine's answer under its name, or that it gave none.
pub struct Answers<'a> {
    pub answers: &'a [Answered],
    /// A blank line between machines.
    pub spaced: bool,
}

impl fmt::Display for Answers<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, Answered { machine, answer }) in self.answers.iter().enumerate() {
            if self.spaced && i > 0 {
                writeln!(f)?;
            }
            writeln!(f, "{machine}")?;
            match answer.output.is_empty() {
                true => writeln!(f, "  no answer")?,
                false => write!(
                    f,
                    "{}",
                    Indented {
                        text: &String::from_utf8_lossy(&answer.output),
                        by: "  ",
                    }
                )?,
            }
        }
        Ok(())
    }
}

/// Every line of a text behind a prefix.
pub struct Indented<'a> {
    pub text: &'a str,
    pub by: &'a str,
}

impl fmt::Display for Indented<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.text
            .lines()
            .try_for_each(|line| writeln!(f, "{}{line}", self.by))
    }
}

/// A finished job's log as kept on this computer: its heading, then its last lines.
pub struct KeptLog<'a> {
    pub head: &'a str,
    pub log: &'a Path,
    pub bytes: u64,
    pub text: &'a str,
    /// `DIBS_OUT_LINES` as given, which the heading repeats.
    pub lines: &'a str,
    pub last: usize,
}

impl fmt::Display for KeptLog<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.head)?;
        writeln!(
            f,
            "  kept on this computer: {}  ({} bytes, last {} lines)",
            self.log.display(),
            self.bytes,
            self.lines
        )?;
        let all: Vec<&str> = self.text.lines().collect();
        let tail = Indented {
            text: &all[all.len().saturating_sub(self.last)..].join("\n"),
            by: "  | ",
        };
        write!(f, "{tail}")
    }
}
