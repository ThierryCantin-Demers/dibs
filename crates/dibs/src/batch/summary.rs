use super::{
    base::{State, jobs},
    parse::Step,
};
use std::path::Path;

pub fn duration(s: u64) -> String {
    match s {
        s if s >= 3600 => format!("{}h{:02}m", s / 3600, s / 60 % 60),
        s if s >= 60 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

pub fn summary(
    id: &str,
    steps: &[Step],
    machines: &[String],
    states: &[State],
    dir: &Path,
    seconds: u64,
    cancelled: Option<&str>,
) -> String {
    let failed = states
        .iter()
        .filter(|s| matches!(s, State::Done { exit, .. } if *exit != 0))
        .count();
    let not_run = states.iter().filter(|s| **s == State::NotRun).count();
    let mut out = format!("batch {id}  {} steps", steps.len());
    if let Some(how) = cancelled {
        out.push_str(&format!(", cancelled {how}"));
    }
    if failed > 0 {
        out.push_str(&format!(", {failed} failed"));
    }
    if not_run > 0 {
        out.push_str(&format!(", {not_run} not run"));
    }
    out.push_str(&format!(", {}\n", duration(seconds)));
    let w = steps.iter().map(|s| s.name.len()).max().unwrap_or(4).max(4);
    let m = machines.iter().map(String::len).max().unwrap_or(7).max(7);
    out.push_str(&format!(
        "{:w$}  {:m$}  {:6}  {:>7}  {:>6}  jobs\n",
        "name", "machine", "lock", "wall", "exit"
    ));
    for (i, st) in steps.iter().enumerate() {
        let err = std::fs::read_to_string(dir.join(format!("{}.err", st.name))).unwrap_or_default();
        let (wall, exit) = match &states[i] {
            State::Done { exit: -1, seconds } => (duration(*seconds), "killed".into()),
            State::Done { exit, seconds } => (duration(*seconds), exit.to_string()),
            _ => ("-".into(), "not run".into()),
        };
        out.push_str(&format!(
            "{:w$}  {:m$}  {:6}  {:>7}  {:>6}  {}\n",
            st.name,
            machines[i],
            st.lock,
            wall,
            exit,
            jobs(&err).join(", ")
        ));
    }
    out.push_str(&format!(
        "each step's output: {}/<name>.out and .err; a job's whole log: dibs --out <job>\n",
        dir.display()
    ));
    out
}
