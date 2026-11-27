use std::fmt::{self, Write as _};

/// Bytes as lowercase hex, as digests and unguessable names are written. A precision cuts it:
/// `{:.10}` writes the first ten digits.
///
/// # Guarantees
///
/// - Each byte is two lowercase digits, in the bytes' order.
#[derive(Debug, Clone, Copy)]
pub struct Hex<'a>(&'a [u8]);

impl<'a> Hex<'a> {
    pub fn new(bytes: &'a [u8]) -> Hex<'a> {
        Hex(bytes)
    }
}

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&hex_text(self.0))
    }
}

fn hex_text(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, b| {
        let _ = write!(text, "{b:02x}");
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_two_lowercase_digits_each() {
        assert_eq!(Hex::new(&[0x00, 0x0a, 0xff]).to_string(), "000aff");
        assert_eq!(Hex::new(&[]).to_string(), "");
    }

    #[test]
    fn a_precision_keeps_that_many_digits() {
        assert_eq!(format!("{:.3}", Hex::new(&[0xab, 0xcd])), "abc");
    }
}
