/// Text as a machine files it.
///
/// # Guarantees
///
/// - It holds only `[A-Za-z0-9._-]`: every other character of the text became `_`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileName(String);

impl FileName {
    pub fn of_text(text: &str) -> FileName {
        FileName(text.chars().map(filed).collect())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<FileName> for String {
    fn from(name: FileName) -> String {
        name.0
    }
}

fn filed(c: char) -> char {
    match c.is_ascii_alphanumeric() || "._-".contains(c) {
        true => c,
        false => '_',
    }
}
