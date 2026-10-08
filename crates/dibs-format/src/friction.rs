use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// One line of `friction.jsonl`: what got in the way, from the session that hit it. The field
/// order is the line's, which is alphabetical.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrictionNote {
    #[serde(default)]
    pub by: String,
    /// The dibs that was running, by commit.
    #[serde(default, rename = "dibs")]
    pub version: String,
    /// Where it was filed, when `DIBS_REPORTS` names a repo to file it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<u64>,
    #[serde(default, rename = "t")]
    pub when: u64,
    #[serde(default)]
    pub text: String,
}

impl FrictionNote {
    /// Text past these lengths is cut, so one report cannot swamp the list it is read in.
    const TEXT_CHARS: usize = 280;
    const BY_CHARS: usize = 48;
    const VERSION_CHARS: usize = 40;

    /// Whitespace collapsed to one line, because what makes the list readable later is that each
    /// report is one, and two sessions reporting the same thing have to land on the same text to
    /// be counted as two. None when there is no text left.
    pub fn new(text: &str, by: &str, version: &str, when: u64) -> Option<FrictionNote> {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return None;
        }
        Some(FrictionNote {
            when,
            text: text.chars().take(Self::TEXT_CHARS).collect(),
            by: by.chars().take(Self::BY_CHARS).collect(),
            version: version.chars().take(Self::VERSION_CHARS).collect(),
            issue: None,
        })
    }

    /// The line `friction.jsonl` holds for this note, without its newline.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("a friction note serializes")
    }
}

impl FromStr for FrictionNote {
    type Err = serde_json::Error;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lines_friction_jsonl_holds_read_and_write_back_byte_for_byte() {
        for line in [
            r#"{"by":"session suite","dibs":"abc1234","t":1759300000,"text":"a line about what got in the way"}"#,
            r#"{"by":"session suite","dibs":"abc1234","issue":12,"t":1759300000,"text":"filed \"there\""}"#,
        ] {
            assert_eq!(line.parse::<FrictionNote>().unwrap().to_line(), line);
        }
    }

    #[test]
    fn a_report_is_one_line_however_it_was_typed() {
        let note = FrictionNote::new(
            "  the trailer says built=nothing\n  but it did build  ",
            "a",
            "abc",
            10,
        )
        .unwrap();
        assert_eq!(note.text, "the trailer says built=nothing but it did build");
        assert!(FrictionNote::new("   ", "a", "abc", 10).is_none());
    }

    #[test]
    fn a_line_missing_fields_reads_with_them_empty() {
        let note: FrictionNote = r#"{"text":"only this"}"#.parse().unwrap();
        assert_eq!(
            (note.text.as_str(), note.when, note.issue),
            ("only this", 0, None)
        );
    }
}
