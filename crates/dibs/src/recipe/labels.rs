use super::manifest::Step;

pub(crate) fn run_label(
    repo: &str,
    verb: &str,
    name: Option<&str>,
    device: Option<&str>,
) -> String {
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

/// One label per step, suffixed only where it has to be. A recipe with a build and a
/// measurement needs no suffix, because the lock already separates them.
pub(crate) fn label_steps(base: &str, steps: &[Step]) -> Vec<String> {
    let mut out = Vec::with_capacity(steps.len());
    for (i, s) in steps.iter().enumerate() {
        let same = steps.iter().filter(|o| o.lock == s.lock).count();
        if same > 1 {
            out.push(format!("{base}.{}", i + 1));
        } else {
            out.push(base.to_string());
        }
    }
    out
}
