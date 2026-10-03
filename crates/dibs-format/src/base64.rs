//! Base64 as the `base64` tool writes it, which is how a job's files cross to the client.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// The tool wraps its lines at this width.
const LINE: usize = 76;

/// On one line.
pub fn encode(bytes: &[u8]) -> String {
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

/// In lines of the tool's width, each ending in a newline.
pub fn encode_lines(bytes: &[u8]) -> String {
    let one = encode(bytes);
    let mut out = String::with_capacity(one.len() + one.len() / LINE + 1);
    for line in one.as_bytes().chunks(LINE) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push('\n');
    }
    out
}

/// Back to bytes, whatever whitespace is between; None when it is not base64.
pub fn decode(text: &[u8]) -> Option<Vec<u8>> {
    let digits: Vec<u8> = text
        .iter()
        .filter(|b| !b.is_ascii_whitespace() && **b != b'=')
        .map(|b| ALPHABET.iter().position(|a| a == b).map(|p| p as u8))
        .collect::<Option<_>>()?;
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, d)| n | u32::from(*d) << (18 - 6 * i));
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
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
            assert_eq!(
                decode(encode(text.as_bytes()).as_bytes()).as_deref(),
                Some(text.as_bytes())
            );
        }
        assert_eq!(decode(b"Zm9v\nYmFy\n").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(decode(b"not base64!"), None);
    }

    #[test]
    fn it_pads_and_wraps_as_the_tool_does() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        let wrapped = encode_lines(&[0u8; 60]);
        assert_eq!(
            wrapped.lines().map(str::len).collect::<Vec<_>>(),
            vec![76, 4]
        );
        assert_eq!(decode(wrapped.as_bytes()).as_deref(), Some(&[0u8; 60][..]));
    }
}
