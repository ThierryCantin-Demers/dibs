use std::fmt::Write as _;

/// Bytes as lowercase hex, as digests and unguessable names are written.
pub trait Hex {
    fn hex(&self) -> String;
}

impl Hex for [u8] {
    fn hex(&self) -> String {
        self.iter().fold(String::new(), |mut text, b| {
            let _ = write!(text, "{b:02x}");
            text
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_two_lowercase_digits_each() {
        assert_eq!([0x00, 0x0a, 0xff].hex(), "000aff");
        assert_eq!([].hex(), "");
    }
}
