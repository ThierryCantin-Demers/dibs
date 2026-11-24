//! A step whose call let go of its job: the calls that wait on that job and stop it, read from
//! what the step's call said.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hint {
    /// What follows `dibs` in the call that waits on the job, or on the batch the step drives.
    pub follow: Vec<String>,
    /// What follows `dibs` in the call that stops it.
    pub kill: Option<Vec<String>>,
}

impl Hint {
    /// The last calls of the shapes dibs prints at a check-in, every word checked: a job prints
    /// what it likes, and none of it may become a command.
    pub fn read(said: &str) -> Option<Hint> {
        let call = |ends: &str, shape: fn(&[&str]) -> bool| {
            said.lines().rev().find_map(|line| {
                let (call, _) = line.trim_start().split_once(ends)?;
                let words: Vec<&str> = call.split(' ').filter(|w| !w.is_empty()).collect();
                let (dibs, words) = words.split_first()?;
                (*dibs == "dibs" && shape(words))
                    .then(|| words.iter().map(|w| w.to_string()).collect())
            })
        };
        let hint = Hint {
            follow: call("  waits for ", follows)?,
            kill: call("  stops ", kills),
        };
        // A job's follow names the job the runner's check-in did.
        let checked = said.lines().rev().find_map(|line| {
            let rest = line.split("dibs: job ").nth(1)?;
            rest.contains("and still runs")
                .then(|| rest.split(' ').next())
                .flatten()
        });
        match hint.job() {
            Some(job) if checked != Some(job) => None,
            _ => Some(hint),
        }
    }

    /// The job its follow waits on, when it is one job rather than a batch.
    pub fn job(&self) -> Option<&str> {
        match self.follow.as_slice() {
            [.., follow, job] if follow == "--follow" && is_job(job) => Some(job),
            _ => None,
        }
    }

    /// A step's line for the call, whose every word `read` checked.
    pub fn line(words: &[String]) -> String {
        format!("dibs {}", words.join(" "))
    }
}

fn follows(words: &[&str]) -> bool {
    match words {
        ["batch", "--follow", id] => is_batch(id),
        ["--on", machine, "--follow", job] => is_machine(machine) && is_job(job),
        ["--follow", job] => is_job(job),
        _ => false,
    }
}

fn kills(words: &[&str]) -> bool {
    let target = |t: &str| is_batch(t) || t.bytes().all(|b| b.is_ascii_digit());
    match words {
        ["--on", machine, "--kill", t] => is_machine(machine) && target(t),
        ["--kill", t] => target(t),
        _ => false,
    }
}

/// `<digits>-<digits>`.
fn is_job(word: &str) -> bool {
    numbers(word) == Some(2)
}

/// `<digits>-<digits>-<digits>`.
pub fn is_batch(word: &str) -> bool {
    numbers(word) == Some(3)
}

fn numbers(word: &str) -> Option<usize> {
    let parts: Vec<&str> = word.split('-').collect();
    parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        .then_some(parts.len())
}

fn is_machine(word: &str) -> bool {
    !word.starts_with('-')
        && !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@:".contains(&b))
}
