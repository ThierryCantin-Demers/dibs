pub fn run_label(repo: &str, verb: &str, name: Option<&str>, device: Option<&str>) -> String {
    let base = match name {
        Some(n) => format!("{repo}/{verb}/{n}"),
        None => format!("{repo}/{verb}"),
    };
    // On a machine with one card the device adds nothing, and on a machine with four it is
    // the difference between four histories and one. Without it a recipe named one series
    // per card, so the second card was refused and --new-series answered by discarding the
    // first: two cards could be measured, never both kept.
    match device {
        Some(d) => format!("{base}@{d}"),
        None => base,
    }
}
