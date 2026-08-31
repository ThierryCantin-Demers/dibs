//! Reading the run records back.
//!
//! Recording provenance is half of it. The half that matters is being able to ask whether two
//! numbers are comparable, because a label alone never answered that: the same name can cover
//! two different procedures at two different refs, and averaging across that is the failure
//! the history exists to prevent.
//!
//! So this does not just list. It says when a label's runs stopped being comparable, and
//! where.

use std::collections::BTreeMap;
use std::path::Path;

pub struct Record {
    /// Kept so a record can be placed in time when something needs explaining, even though
    /// the report orders by position in the file rather than printing a clock.
    #[allow(dead_code)]
    pub when: u64,
    pub verb: String,
    pub label: String,
    pub fingerprint: String,
    pub isolation: String,
    pub revisions: Vec<(String, String)>,
    pub seconds: u64,
    pub failed: bool,
    pub reason: Option<String>,
}

/// Hand-rolled rather than pulled through serde: the file is append-only and written by this
/// program, so a line that does not parse is a corrupted tail rather than a schema question,
/// and skipping it is the right answer.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":\"");
    let i = line.find(&pat)? + pat.len();
    let rest = &line[i..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn number(line: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\":");
    let i = line.find(&pat)? + pat.len();
    let rest = &line[i..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn parse_line(line: &str) -> Option<Record> {
    let revs_start = line.find("\"revisions\":{")? + "\"revisions\":{".len();
    let revs_end = revs_start + line[revs_start..].find('}')?;
    let mut revisions = Vec::new();
    for pair in line[revs_start..revs_end].split("\",\"") {
        let cleaned = pair.trim_matches(|c| c == '"');
        if let Some((k, v)) = cleaned.split_once("\":\"") {
            revisions.push((k.trim_matches('"').to_string(), v.trim_matches('"').to_string()));
        }
    }
    // Sum the steps rather than the whole run: what a comparison cares about is the work, and
    // a job that queued for twenty minutes did not take twenty minutes.
    let mut seconds = 0;
    let mut failed = false;
    let steps = line.find("\"steps\":[").map(|i| &line[i..]).unwrap_or("");
    for chunk in steps.split("{\"lock\"").skip(1) {
        seconds += number(chunk, "seconds").unwrap_or(0);
        if number(chunk, "status").unwrap_or(0) != 0 {
            failed = true;
        }
    }
    Some(Record {
        when: number(line, "t")?,
        verb: field(line, "verb")?.to_string(),
        label: field(line, "label")?.to_string(),
        fingerprint: field(line, "fingerprint").unwrap_or("").to_string(),
        isolation: field(line, "isolation").unwrap_or("").to_string(),
        revisions,
        seconds,
        failed,
        reason: field(line, "reason").map(str::to_string),
    })
}

pub fn load(path: &Path) -> Result<Vec<Record>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    Ok(text.lines().filter(|l| !l.trim().is_empty()).filter_map(parse_line).collect())
}

pub fn report(records: &[Record], only: Option<&str>, limit: usize) -> String {
    let mut out = String::new();
    let shown: Vec<&Record> = records
        .iter()
        .rev()
        .filter(|r| only.is_none_or(|l| r.label == l || r.label.starts_with(&format!("{l}/"))))
        .take(limit)
        .collect();

    if shown.is_empty() {
        return match only {
            Some(l) => format!("nothing recorded for {l}\n"),
            None => "nothing recorded yet\n".to_string(),
        };
    }

    for r in &shown {
        let revs: Vec<String> =
            r.revisions.iter().map(|(k, v)| format!("{k}@{v}")).collect();
        out.push_str(&format!(
            "{:<7} {:<28} {:>6}s  {:<9} {}{}\n",
            r.verb,
            r.label,
            r.seconds,
            r.isolation,
            revs.join(" "),
            if r.failed { "  FAILED" } else { "" }
        ));
    }

    // The point of recording the fingerprint. A label whose procedure changed has a history
    // that is two histories, and nothing else would say so.
    let mut by_label: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
    for r in records {
        by_label.entry(&r.label).or_default().push(r);
    }
    let mut split = Vec::new();
    for (label, rs) in &by_label {
        if only.is_some_and(|l| !(label == &l || label.starts_with(&format!("{l}/")))) {
            continue;
        }
        let mut seen: Vec<&str> = rs.iter().map(|r| r.fingerprint.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        if seen.len() > 1 {
            split.push((*label, seen.len()));
        }
    }
    if !split.is_empty() {
        out.push('\n');
        for (label, n) in split {
            out.push_str(&format!(
                "  {label}: {n} different recipes have run under this name. Runs made under \
                 one are not comparable with runs made under another.\n"
            ));
        }
    }
    out
}

/// What did not fit, grouped. The escape hatch is instrumented rather than discouraged: a
/// reason that keeps recurring is a specification for the next verb, written by whoever needed
/// it rather than guessed at here.
///
/// Raw usage is not a number to drive to zero. A one-off sweep written for one investigation
/// is a benchmark that will never run again, and forcing a recipe for it is friction with no
/// payoff. The failure to watch for is the opposite: a reason that recurs and nobody promoted.
pub fn gaps(records: &[Record]) -> String {
    let mut counts: BTreeMap<&str, (usize, Vec<&str>)> = BTreeMap::new();
    for r in records {
        if let Some(reason) = &r.reason {
            let e = counts.entry(reason.as_str()).or_insert((0, Vec::new()));
            e.0 += 1;
            if !e.1.contains(&r.label.as_str()) {
                e.1.push(&r.label);
            }
        }
    }
    if counts.is_empty() {
        return "Nothing has needed the escape hatch. Either everything fits a recipe, or nobody\nis using the verbs yet.\n"
            .to_string();
    }
    let mut ordered: Vec<(&str, usize, Vec<&str>)> =
        counts.into_iter().map(|(k, (n, l))| (k, n, l)).collect();
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

    let mut out = String::from("What did not fit a recipe:\n\n");
    for (reason, n, labels) in &ordered {
        out.push_str(&format!("  {n:>3}x  {reason}\n       {}\n", labels.join(", ")));
    }
    let repeated = ordered.iter().filter(|(_, n, _)| *n > 1).count();
    if repeated > 0 {
        out.push_str(&format!(
            "\n{repeated} of these have happened more than once. Those are the ones worth a \
             recipe;\nthe rest are one-offs and are meant to stay here.\n"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = r#"{"t":100,"verb":"bench","label":"cubek/gemm","fingerprint":"aaa","isolation":"machine","backend":"dibs","revisions":{"cubek":"abc123"},"steps":[{"lock":"shared","status":0,"seconds":30},{"lock":"exclusive","status":0,"seconds":120}]}"#;
    const B: &str = r#"{"t":200,"verb":"bench","label":"cubek/gemm","fingerprint":"bbb","isolation":"machine","backend":"dibs","revisions":{"cubek":"def456"},"steps":[{"lock":"exclusive","status":1,"seconds":5}]}"#;

    #[test]
    fn a_record_reads_back_whole() {
        let r = parse_line(A).unwrap();
        assert_eq!(r.label, "cubek/gemm");
        assert_eq!(r.revisions, vec![("cubek".into(), "abc123".into())]);
        // The work, not the wall clock: both steps summed.
        assert_eq!(r.seconds, 150);
        assert!(!r.failed);
    }

    #[test]
    fn a_failing_step_marks_the_run() {
        assert!(parse_line(B).unwrap().failed);
    }

    #[test]
    fn a_label_whose_recipe_changed_is_called_out() {
        let recs = vec![parse_line(A).unwrap(), parse_line(B).unwrap()];
        let out = report(&recs, None, 10);
        assert!(out.contains("2 different recipes have run under this name"),
                "a changed procedure has to be reported, or two histories average silently");
    }

    #[test]
    fn one_recipe_throughout_says_nothing() {
        let recs = vec![parse_line(A).unwrap()];
        assert!(!report(&recs, None, 10).contains("different recipes"));
    }

    #[test]
    fn gaps_ranks_what_recurs_and_says_which_to_promote() {
        let mk = |reason: &str| {
            let line = format!(
                r#"{{"t":1,"verb":"raw","label":"raw","fingerprint":"","isolation":"machine","reason":"{reason}","backend":"dibs","revisions":{{}},"steps":[]}}"#
            );
            parse_line(&line).unwrap()
        };
        let recs = vec![mk("git bisect across builds"), mk("git bisect across builds"), mk("one off")];
        let out = gaps(&recs);
        assert!(out.starts_with("What did not fit"));
        // The recurring one first, because that is the one worth a recipe.
        let bisect = out.find("git bisect").unwrap();
        let oneoff = out.find("one off").unwrap();
        assert!(bisect < oneoff, "the recurring reason has to rank above the one-off");
        assert!(out.contains("1 of these have happened more than once"));
    }

    #[test]
    fn no_reasons_recorded_is_not_an_error() {
        assert!(gaps(&[]).contains("Nothing has needed the escape hatch"));
    }

    #[test]
    fn a_corrupt_tail_is_skipped_rather_than_fatal() {
        assert!(parse_line("{\"t\":1,\"verb\":\"bench\"").is_none());
    }
}
