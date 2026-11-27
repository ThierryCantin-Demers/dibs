use std::{borrow::Cow, fmt};

/// What a field holds when there is nothing to say.
pub const DASH: &str = "-";

/// One field of a line as it is written.
///
/// It writes a tab, a newline or a carriage return as a space even where the runner cleaned the
/// text already, as a defence against a request from any other client.
pub struct Field<'a> {
    text: Cow<'a, str>,
}

impl<'a> Field<'a> {
    pub fn from_text(text: &'a str) -> Field<'a> {
        Field {
            text: Cow::Borrowed(text),
        }
    }

    /// Free text, written as `-` when empty.
    pub fn from_filled(text: &'a str) -> Field<'a> {
        Field {
            text: Cow::Borrowed(filled(text)),
        }
    }

    /// Free text, written as `-` when absent or empty.
    pub fn from_filled_optional(text: Option<&'a str>) -> Field<'a> {
        Field::from_filled(text.unwrap_or_default())
    }

    /// A value, written as `-` when absent or when it writes as nothing.
    pub fn from_dashed<T: fmt::Display>(value: Option<&T>) -> Field<'a> {
        Field {
            text: dashed(value),
        }
    }

    /// Free text, written empty when absent.
    pub fn from_optional(text: Option<&'a str>) -> Field<'a> {
        Field {
            text: Cow::Borrowed(text.unwrap_or_default()),
        }
    }
}

impl fmt::Display for Field<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_field(f, &self.text)
    }
}

fn filled(text: &str) -> &str {
    match text.is_empty() {
        true => DASH,
        false => text,
    }
}

fn dashed<T: fmt::Display>(value: Option<&T>) -> Cow<'static, str> {
    value
        .map(ToString::to_string)
        .filter(|text| !text.is_empty())
        .map_or(Cow::Borrowed(DASH), Cow::Owned)
}

fn write_field(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    for c in text.chars() {
        match c {
            '\t' | '\n' | '\r' => f.write_str(" ")?,
            c => fmt::Write::write_char(f, c)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_a_newline_or_a_carriage_return_in_free_text_is_written_as_a_space() {
        for breaking in ['\t', '\n', '\r'] {
            let text = format!("a{breaking}b");
            assert_eq!(Field::from_text(&text).to_string(), "a b");
        }
    }

    #[test]
    fn empty_filled_text_is_written_as_a_dash() {
        assert_eq!(Field::from_filled("").to_string(), DASH);
    }

    #[test]
    fn filled_text_is_written_as_itself() {
        assert_eq!(Field::from_filled("cmd").to_string(), "cmd");
    }

    #[test]
    fn absent_filled_text_is_written_as_a_dash() {
        assert_eq!(Field::from_filled_optional(None).to_string(), DASH);
    }

    #[test]
    fn an_absent_value_is_written_as_a_dash() {
        assert_eq!(Field::from_dashed::<u64>(None).to_string(), DASH);
    }

    #[test]
    fn a_value_that_writes_as_nothing_is_written_as_a_dash() {
        assert_eq!(Field::from_dashed(Some(&"")).to_string(), DASH);
    }

    #[test]
    fn a_dashed_value_is_written_as_itself() {
        assert_eq!(Field::from_dashed(Some(&3)).to_string(), "3");
    }

    #[test]
    fn absent_optional_text_is_written_empty() {
        assert_eq!(Field::from_optional(None).to_string(), "");
    }
}
