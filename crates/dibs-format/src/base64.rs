//! Base64 as the `base64` tool writes it, which is how a job's files cross to the client.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// The tool wraps its lines at this width.
const LINE_CHARS: usize = 76;
/// Each byte's digit in `ALPHABET`, or `NOT_A_DIGIT`.
const DIGITS: [u8; 256] = digits();
const NOT_A_DIGIT: u8 = 0xff;

/// Bytes encoded in base64, padded with `=`.
///
/// # Guarantees
///
/// - Its text is what `base64 -w0` writes for the same bytes, and its lines what `base64` writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base64(String);

impl Base64 {
    pub fn encode(bytes: &[u8]) -> Base64 {
        Base64(encoded(bytes))
    }

    /// On one line.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// In lines of the tool's width, each ending in a newline.
    pub fn to_lines(&self) -> String {
        wrapped(&self.0)
    }

    /// Back to bytes, whatever whitespace is between; None when it is not base64: a character
    /// outside the alphabet, `=` anywhere but at the end, or a last group of one digit.
    pub fn decode(text: &[u8]) -> Option<Vec<u8>> {
        decoded(text)
    }
}

const fn digits() -> [u8; 256] {
    let mut table = [NOT_A_DIGIT; 256];
    let mut i = 0;
    while i < ALPHABET.len() {
        table[ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    table
}

fn encoded(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            match i <= chunk.len() {
                true => out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63])),
                false => out.push('='),
            }
        }
    }
    out
}

fn wrapped(one_line: &str) -> String {
    let mut out = String::with_capacity(one_line.len() + one_line.len() / LINE_CHARS + 1);
    for line in one_line.as_bytes().chunks(LINE_CHARS) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push('\n');
    }
    out
}

fn decoded(text: &[u8]) -> Option<Vec<u8>> {
    let text: Vec<u8> = text
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let padding = text.iter().rev().take_while(|b| **b == b'=').count();
    let digits: Vec<u8> = text[..text.len() - padding]
        .iter()
        .map(|b| Some(DIGITS[usize::from(*b)]).filter(|d| *d != NOT_A_DIGIT))
        .collect::<Option<_>>()?;
    if padding > 2 || digits.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, d)| n | u32::from(*d) << (18 - 6 * i));
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_encoded_decodes_back() {
        for text in [
            "",
            "f",
            "fo",
            "foo",
            "foobar",
            "a longer line\n with a newline",
        ] {
            let encoded = Base64::encode(text.as_bytes());
            assert_eq!(
                Base64::decode(encoded.as_str().as_bytes()).as_deref(),
                Some(text.as_bytes())
            );
        }
    }

    #[test]
    fn whitespace_between_digits_is_skipped() {
        assert_eq!(
            Base64::decode(b"Zm9v\nYmFy\n").as_deref(),
            Some(&b"foobar"[..])
        );
    }

    #[test]
    fn text_outside_the_alphabet_is_not_base64() {
        assert_eq!(Base64::decode(b"not base64!"), None);
    }

    #[test]
    fn padding_before_the_end_is_not_base64() {
        assert_eq!(Base64::decode(b"Zg==Zg=="), None);
    }

    #[test]
    fn a_last_group_of_one_digit_is_not_base64() {
        assert_eq!(Base64::decode(b"Zm9vY"), None);
    }

    #[test]
    fn it_pads_as_the_tool_does() {
        assert_eq!(Base64::encode(b"").as_str(), "");
        assert_eq!(Base64::encode(b"f").as_str(), "Zg==");
        assert_eq!(Base64::encode(b"fo").as_str(), "Zm8=");
        assert_eq!(Base64::encode(b"foo").as_str(), "Zm9v");
        assert_eq!(Base64::encode(b"foobar").as_str(), "Zm9vYmFy");
    }

    #[test]
    fn it_wraps_as_the_tool_does() {
        let wrapped = Base64::encode(&[0u8; 60]).to_lines();
        assert_eq!(
            wrapped.lines().map(str::len).collect::<Vec<_>>(),
            vec![76, 4]
        );
    }
}
