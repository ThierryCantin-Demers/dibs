use serde::Serialize;

/// A document as one line of JSON and its newline, as a reader taking a line at a time reads it.
pub fn json_line<T: Serialize>(document: &T) -> String {
    format!(
        "{}\n",
        serde_json::to_string(document).expect("a document serializes")
    )
}
