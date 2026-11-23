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
        Some(d) => format!("{base}@{}", crate::machine::Card::spelled(d)),
        None => base,
    }
}

/// What a shell run is filed under within its repo: the work `--label` names, else its command,
/// as its program's first words and a hash of the whole, so a command run again is one series
/// and a changed one starts another.
pub fn shell_name(label: Option<&str>, command: &str) -> String {
    if let Some(label) = label {
        return label.to_string();
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    let mut at = 0;
    while at < words.len() {
        match words[at] {
            "cd" => at += 2,
            "&&" | ";" | "exec" | "env" | "time" => at += 1,
            word if word.contains('=') && !word.starts_with('-') => at += 1,
            _ => break,
        }
    }
    let stem: String = words
        .iter()
        .skip(at)
        .take(2)
        .map(|w| {
            w.rsplit('/')
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let hash = command.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let hash = format!("{:012x}", hash >> 16);
    match stem.is_empty() {
        true => hash,
        false => format!("{stem}-{hash}"),
    }
}

/// Whether `label` is filed as written: anything else would be filed under a name another label
/// shares, and `dibs runs` would not find it under the one given.
pub fn files_as_written(label: &str) -> bool {
    !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}
