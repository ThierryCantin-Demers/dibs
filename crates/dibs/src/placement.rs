//! Where shared work that names no machine goes: ranked by load, a repo's work kept on the
//! machine holding its build cache. A stale reading costs a worse queue, never a double booking.

use crate::{
    call::{Asked, Bound, MachineCall},
    cli::Call,
    machine::Kept,
    render::Answered,
};
use dibs_format::{Exit, MachineName, Mode};
use serde::Deserialize;
use std::{
    collections::hash_map::RandomState,
    fmt::{self, Write as _},
    hash::{BuildHasher as _, Hasher as _},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const PICK_POLL_SECS: u64 = 5;
/// How long a machine that gave no answer is left out of the ranking.
const DEFAULT_BACKOFF_SECS: u64 = 300;
/// What a workstation's load costs on top, since a build there takes its owner's editor.
const DEFAULT_SELF_PENALTY: u64 = 25;
/// A machine held exclusively cannot start shared work, so it ranks behind every one that can.
const BENCH_PENALTY: u64 = 1000;
/// A cache landing where no benchmark can follow it is a cache in the wrong place.
const UNMEASURED_PENALTY: u64 = 500;

/// What placement reads of a machine's status.
#[derive(Debug, Clone, Deserialize)]
pub struct Reading {
    pub state: LockState,
    pub cores: Option<u64>,
    pub load: Option<u64>,
    pub caches: Option<Vec<String>>,
    /// None from a machine too old to say, which is ranked rather than read as having none.
    pub clones: Option<Vec<String>>,
}

/// Who holds a machine's lock, as its status says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockState {
    Idle,
    Shared,
    Bench,
    Busy,
    Orphan,
    /// One this client does not know, from a newer machine half.
    #[serde(other)]
    Other,
}

impl LockState {
    fn as_str(self) -> &'static str {
        match self {
            LockState::Idle => "idle",
            LockState::Shared => "shared",
            LockState::Bench => "bench",
            LockState::Busy => "busy",
            LockState::Orphan => "orphan",
            LockState::Other => "other",
        }
    }
}

impl fmt::Display for LockState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Reading {
    /// Held exclusively, so it cannot start shared work.
    fn benchmarking(&self) -> bool {
        self.state == LockState::Bench
    }
}

/// A machine that answered, as the ranking sees it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: MachineName,
    pub reading: Reading,
    pub workstation: bool,
    pub measures: bool,
}

/// What the ranking weighs besides the readings.
#[derive(Debug, Clone, Default)]
pub struct Ranking {
    /// The machine holding the repo's cache, as last recorded.
    pub prefer: Option<String>,
    pub repo: Option<String>,
    pub self_penalty: u64,
}

/// Where a ranking put the work, and what it said about each machine.
#[derive(Debug)]
pub struct Ranked {
    pub placed: Result<MachineName, Unplaced>,
    pub said: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unplaced {
    /// Every machine that answered lacks a clone of the repo.
    NoClone {
        repo: String,
    },
    NoneAnswered {
        inventory: String,
    },
}

impl Ranking {
    /// Ranks the machines that answered, in the order given; `coin` breaks ties, so clients
    /// reading the same snapshot do not all pick the same machine.
    pub fn place(&self, candidates: &[Candidate], mut coin: impl FnMut() -> bool) -> Ranked {
        let mut said = String::new();
        let mut best: Option<(&MachineName, u64)> = None;
        let mut cached: Option<(&MachineName, u64)> = None;
        let mut preferred_answered = false;
        for candidate in candidates {
            let name = &candidate.name;
            let reading = &candidate.reading;
            if let (Some(repo), Some(clones)) = (&self.repo, &reading.clones)
                && !clones.contains(repo)
            {
                let _ = writeln!(said, "  {:<18} no clone of {repo}", name.as_str());
                continue;
            }
            let cores = reading.cores.filter(|c| *c > 0).unwrap_or(1);
            let mut score = reading.load.unwrap_or_default() / cores;
            if reading.benchmarking() {
                score += BENCH_PENALTY;
            }
            if candidate.workstation {
                score += self.self_penalty;
            }
            if !candidate.measures {
                score += UNMEASURED_PENALTY;
            }
            let preferred = self.prefer.as_deref() == Some(name.as_str());
            preferred_answered |= preferred;
            let holds = self.repo.as_ref().is_some_and(|repo| {
                reading
                    .caches
                    .as_ref()
                    .is_some_and(|caches| caches.contains(repo))
            });
            if holds && cached.is_none_or(|(_, s)| score < s) {
                cached = Some((name, score));
            }
            let _ = writeln!(
                said,
                "  {:<18} {score}% busy, {}{}{}",
                name.as_str(),
                reading.state,
                match candidate.workstation {
                    true => ", someone works here",
                    false => "",
                },
                match preferred {
                    true => ", holds the cache",
                    false => "",
                }
            );
            let better = match best {
                None => true,
                Some((_, b)) => score < b || (score == b && coin()),
            };
            if better {
                best = Some((name, score));
            }
        }
        let placed = match (&self.prefer, cached, best) {
            (Some(prefer), _, _) if preferred_answered => Ok(MachineName::new(prefer.as_str())),
            (_, Some((cached, _)), _) => Ok(cached.clone()),
            (_, None, Some((best, _))) => Ok(best.clone()),
            (_, None, None) => Err(match (&self.repo, candidates.is_empty()) {
                (Some(repo), false) => Unplaced::NoClone { repo: repo.clone() },
                _ => Unplaced::NoneAnswered {
                    inventory: String::new(),
                },
            }),
        };
        Ranked { placed, said }
    }
}

/// Placement as a call makes it: every machine asked at once, then ranked.
pub struct Placement<'a> {
    pub machine: &'a MachineCall<'a>,
}

impl Placement<'_> {
    /// Says what `-v` asks for on stderr as it goes.
    pub fn pick(&self) -> Result<MachineName, Unplaced> {
        let call = self.machine.call;
        let down = RouteDown::from_env(self.machine.paths.route_down());
        let names = self.machine.fleet.names();
        let mut asked = Vec::new();
        for name in &names {
            match down.recently(name) {
                Some(ago) => {
                    if call.verbose {
                        eprintln!(
                            "  {:<18} no answer {ago}s ago, not asked again yet",
                            name.as_str()
                        );
                    }
                }
                None => asked.push(name.clone()),
            }
        }
        let flags = Call {
            json: true,
            ..Call::default()
        };
        let bound = Bound::polled(PICK_POLL_SECS, Kept::StdoutAlone);
        let mut answers = self.machine.each(&asked, |name| {
            let status = Asked::plain(Mode::Status, self.machine.label());
            self.machine.ask(name, &flags, status, bound)
        });
        answers.sort_by(|a, b| a.machine.as_str().cmp(b.machine.as_str()));
        let mut candidates = Vec::new();
        for Answered {
            machine: name,
            answer,
        } in answers
        {
            if answer.output.is_empty() {
                if call.verbose {
                    eprintln!("  {:<18} no answer", name.as_str());
                }
                down.mark(&name);
                continue;
            }
            down.clear(&name);
            let text = String::from_utf8_lossy(&answer.output);
            let Some(reading) = text
                .lines()
                .find_map(|line| serde_json::from_str::<Reading>(line).ok())
            else {
                continue;
            };
            let entry = self.machine.fleet.entry(name.as_str());
            candidates.push(Candidate {
                workstation: entry.is_some_and(|m| m.workstation),
                measures: entry.is_none_or(|m| m.measure),
                name,
                reading,
            });
        }
        let ranking = Ranking {
            prefer: call.prefer.clone(),
            repo: call.repo.clone(),
            self_penalty: env_number("DIBS_SELF_PENALTY").unwrap_or(DEFAULT_SELF_PENALTY),
        };
        let ranked = ranking.place(&candidates, coin);
        if call.verbose {
            eprint!("{}", ranked.said);
        }
        ranked.placed.map_err(|unplaced| match unplaced {
            Unplaced::NoneAnswered { .. } => Unplaced::NoneAnswered {
                inventory: self.machine.fleet.shown(),
            },
            other => other,
        })
    }
}

/// Machines that gave no answer lately, which are not asked again for a while: one that is down
/// would otherwise cost every dispatch the whole probe timeout.
struct RouteDown {
    dir: Option<PathBuf>,
    backoff: u64,
    now: u64,
}

impl RouteDown {
    fn from_env(dir: Option<PathBuf>) -> RouteDown {
        RouteDown {
            dir,
            backoff: env_number("DIBS_ROUTE_BACKOFF").unwrap_or(DEFAULT_BACKOFF_SECS),
            now: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
        }
    }

    /// How long ago it last gave no answer, while that is within the backoff.
    fn recently(&self, name: &MachineName) -> Option<u64> {
        let text = std::fs::read_to_string(self.dir.as_ref()?.join(name.as_str())).ok()?;
        let when: u64 = text.trim().parse().unwrap_or_default();
        let ago = self.now.saturating_sub(when);
        (ago < self.backoff).then_some(ago)
    }

    fn mark(&self, name: &MachineName) {
        if let Some(dir) = &self.dir
            && std::fs::create_dir_all(dir).is_ok()
        {
            let _ = std::fs::write(dir.join(name.as_str()), format!("{}\n", self.now));
        }
    }

    fn clear(&self, name: &MachineName) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_file(dir.join(name.as_str()));
        }
    }
}

impl Unplaced {
    pub fn exit(&self) -> Exit {
        Exit::Unreachable
    }
}

impl fmt::Display for Unplaced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unplaced::NoClone { repo } => {
                writeln!(
                    f,
                    "dibs: every machine answered, but none has a clone of '{repo}'."
                )?;
                writeln!(
                    f,
                    "  A worktree is prepared from $HOME/prog/{repo} on the machine itself, so one"
                )?;
                writeln!(
                    f,
                    "  has to be cloned there before any work on {repo} can be sent to it."
                )
            }
            Unplaced::NoneAnswered { inventory } => writeln!(
                f,
                "dibs: no machine in {inventory} answered, so there was nowhere to place this."
            ),
        }
    }
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

/// A fair coin, from the hasher seed std draws from the operating system.
fn coin() -> bool {
    RandomState::new().build_hasher().finish() & 1 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(name: &str, state: LockState, load: u64) -> Candidate {
        Candidate {
            name: MachineName::new(name),
            reading: Reading {
                state,
                cores: Some(1),
                load: Some(load),
                caches: Some(Vec::new()),
                clones: Some(vec!["app".into()]),
            },
            workstation: false,
            measures: true,
        }
    }

    #[test]
    fn the_least_loaded_machine_wins_and_a_benchmark_ranks_last() {
        let ranked = Ranking::default().place(
            &[
                machine("a", LockState::Bench, 0),
                machine("b", LockState::Idle, 90),
            ],
            || false,
        );
        assert_eq!(ranked.placed, Ok(MachineName::new("b")));
        assert_eq!(ranked.said.lines().count(), 2);
    }

    #[test]
    fn the_cache_wins_over_load_and_a_missing_clone_is_dropped() {
        let mut cached = machine("a", LockState::Idle, 80);
        cached.reading.caches = Some(vec!["app".into()]);
        let ranking = Ranking {
            repo: Some("app".into()),
            ..Ranking::default()
        };
        let ranked = ranking.place(&[cached, machine("b", LockState::Idle, 0)], || false);
        assert_eq!(ranked.placed, Ok(MachineName::new("a")));
        let ranking = Ranking {
            repo: Some("absent".into()),
            ..Ranking::default()
        };
        assert_eq!(
            ranking
                .place(&[machine("a", LockState::Idle, 0)], || false)
                .placed,
            Err(Unplaced::NoClone {
                repo: "absent".into()
            })
        );
    }
}
