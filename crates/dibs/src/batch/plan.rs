use super::{
    base::{State, StepStderr},
    parse::Step,
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

/// A batch's steps, each with the machine it goes to.
#[derive(Clone, Copy)]
pub struct Batch<'a> {
    pub steps: &'a [Step],
    pub machines: &'a [String],
}

impl Batch<'_> {
    /// One line per step: its machine, its kind and what it waits for.
    pub fn plan(&self) -> String {
        let (steps, machines) = (self.steps, self.machines);
        let w = steps.iter().map(|s| s.name.len()).max().unwrap_or(4).max(4);
        let m = machines.iter().map(String::len).max().unwrap_or(7).max(7);
        let mut s = String::new();
        for (i, st) in steps.iter().enumerate() {
            let after = if st.after.is_empty() {
                "-".to_string()
            } else {
                st.after.join(",")
            };
            s.push_str(&format!(
                "  {:w$}  {:m$}  {:6}  after {}{}\n",
                st.name,
                machines[i],
                st.lock,
                after,
                if st.cont { ", cont" } else { "" }
            ));
        }
        s
    }

    /// The steps that may start now, lowest first. A step waits for everything it names, and for
    /// its machine to have no other step of this batch on it: two steps on one machine overlapping
    /// is the surprise the lock exists to prevent, and nothing is lost by running them in turn.
    pub fn ready(&self, states: &[State], stopped: bool) -> Vec<usize> {
        let (steps, machines) = (self.steps, self.machines);
        if stopped {
            return Vec::new();
        }
        let index: HashMap<&str, usize> = steps
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.as_str(), i))
            .collect();
        let mut busy: HashSet<&str> = states
            .iter()
            .enumerate()
            .filter(|(_, s)| **s == State::Running)
            .map(|(i, _)| machines[i].as_str())
            .collect();
        let mut out = Vec::new();
        for (i, s) in steps.iter().enumerate() {
            if states[i] != State::Waiting || busy.contains(machines[i].as_str()) {
                continue;
            }
            if s.after
                .iter()
                .all(|a| matches!(states[index[a.as_str()]], State::Done { .. }))
            {
                busy.insert(machines[i].as_str());
                out.push(i);
            }
        }
        out
    }

    pub fn summary(
        &self,
        id: &str,
        states: &[State],
        dir: &Path,
        seconds: u64,
        cancelled: Option<&str>,
    ) -> String {
        let (steps, machines) = (self.steps, self.machines);
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
            let err =
                std::fs::read_to_string(dir.join(format!("{}.err", st.name))).unwrap_or_default();
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
                StepStderr(&err).jobs().join(", ")
            ));
        }
        out.push_str(&format!(
            "each step's output: {}/<name>.out and .err; a job's whole log: dibs --out <job>\n",
            dir.display()
        ));
        out
    }
}

fn duration(s: u64) -> String {
    match s {
        s if s >= 3600 => format!("{}h{:02}m", s / 3600, s / 60 % 60),
        s if s >= 60 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}
