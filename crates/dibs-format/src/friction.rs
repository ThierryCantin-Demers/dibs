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
    fn a_line_missing_fields_reads_with_them_empty() {
        let note: FrictionNote = r#"{"text":"only this"}"#.parse().unwrap();
        assert_eq!(
            (note.text.as_str(), note.when, note.issue),
            ("only this", 0, None)
        );
    }
}
