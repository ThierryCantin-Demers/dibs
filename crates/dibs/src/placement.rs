//! Where shared work that names no machine goes: ranked by load, a repo's work kept on the
//! machine holding its build cache. A stale reading costs a worse queue, never a double booking.

use crate::{
    call::{Asked, Bound, MachineCall, Series},
    caller::Caller,
    cli::Call,
    inventory::{Machine, Role},
    machine::{Card, Kept},
    render::Answered,
};
use dibs_format::{Alias, Exit, Label, MachineName, Mode, Moment, status::SeriesHere};
use serde::Deserialize;
use std::{
    collections::hash_map::RandomState,
    fmt::{self, Write as _},
    hash::{BuildHasher as _, Hasher as _},
    path::PathBuf,
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

/// A job in a machine's status, as a pick of cards reads it.
#[derive(Debug, Clone, Deserialize)]
pub struct Occupant {
    #[serde(default)]
    pub mode: Option<Mode>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub label: String,
}

/// What placement reads of a machine's status.
#[derive(Debug, Clone, Deserialize)]
pub struct Reading {
    #[serde(default)]
    pub holders: Vec<Occupant>,
    #[serde(default)]
    pub queue: Vec<Occupant>,
    pub state: LockState,
    pub cores: Option<u64>,
    pub load: Option<u64>,
    pub caches: Option<Vec<String>>,
    /// None from a machine too old to say, which is ranked rather than read as having none.
    pub clones: Option<Vec<String>>,
    /// Taken back by its owners, so nothing placed there would start.
    #[serde(default)]
    pub closed: Option<serde::de::IgnoredAny>,
    /// The labels measured there, whoever measured them.
    #[serde(default)]
    pub series: Vec<SeriesHere>,
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
    /// A named machine's reading, or None when it gives none in the time a pick allows.
    pub fn asked(name: &MachineName) -> Option<Reading> {
        let call = Call {
            on: Some(name.clone()),
            json: true,
            ..Call::default()
        };
        let caller = Caller::default();
        let machine = MachineCall::new(&call, &caller).ok()?;
        let status = Asked::plain(Mode::Status, machine.label());
        let bound = Bound::polled(PICK_POLL_SECS, Kept::StdoutAlone);
        Reading::answered(&machine.ask(name, &call, status, bound).output)
    }

    fn answered(output: &[u8]) -> Option<Reading> {
        String::from_utf8_lossy(output)
            .lines()
            .find_map(|line| serde_json::from_str(line).ok())
    }

    /// The cards each benchmark running or queued under `label` named: shared jobs of a label
    /// spread over the model's cards, and a series follows only its measurements.
    pub fn bench_cards_of(&self, label: &str) -> impl Iterator<Item = &str> {
        self.holders
            .iter()
            .chain(&self.queue)
            .filter(move |o| o.label == label && o.mode == Some(Mode::Bench))
            .filter_map(|o| o.device.as_deref())
    }

    /// How many jobs running or queued name `card`.
    pub fn using(&self, card: &str) -> u64 {
        self.holders
            .iter()
            .chain(&self.queue)
            .filter(|o| {
                o.device
                    .as_deref()
                    .is_some_and(|d| d.split(',').any(|c| c == card))
            })
            .count() as u64
    }

    /// Held exclusively, so it cannot start shared work.
    fn benchmarking(&self) -> bool {
        self.state == LockState::Bench
    }

    /// When `label` was last measured there, by anyone.
    fn measured(&self, label: &str) -> Option<u64> {
        self.series
            .iter()
            .filter(|s| s.label == label)
            .map(|s| s.at)
            .max()
    }

    /// The cards the newest series of any of `labels` was measured on there, as its runner
    /// records it, whoever measured it.
    pub fn series_cards_of(&self, labels: &[Label]) -> impl Iterator<Item = &str> {
        let newest = self
            .series
            .iter()
            .filter(|s| labels.iter().any(|l| l.filed().as_str() == s.label) && s.card != "none")
            .max_by_key(|s| s.at);
        newest.map(|s| s.card.as_str()).into_iter()
    }

    /// Whether a benchmark of `label` runs or waits there, on any card or none.
    fn measuring(&self, label: &str) -> bool {
        self.holders
            .iter()
            .chain(&self.queue)
            .any(|o| o.label == label && o.mode == Some(Mode::Bench))
    }

    /// Benchmarks running or waiting there.
    fn benchmarks(&self) -> u64 {
        self.holders
            .iter()
            .chain(&self.queue)
            .filter(|o| o.mode == Some(Mode::Bench))
            .count() as u64
    }
}

/// A machine that answered, as the ranking sees it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: MachineName,
    /// None from a machine that answered without one: it has no runner for this dibs yet, which
    /// a question never builds and the call placed there does.
    pub reading: Option<Reading>,
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
    /// `--device` named a card no machine's entry lists.
    NoCard {
        alias: Alias,
    },
    /// The inventory gives roles, and no machine that could take the work has the one it needs.
    NoRole {
        role: Role,
    },
}

impl Ranking {
    /// Ranks the machines that answered, in the order given; `coin` breaks ties, so clients
    /// reading the same snapshot do not all pick the same machine.
    pub fn place(&self, candidates: &[Candidate], mut coin: impl FnMut() -> bool) -> Ranked {
        let mut said = String::new();
        let mut best: Option<(&MachineName, u64)> = None;
        let mut cached: Option<(&MachineName, u64)> = None;
        let mut unread: Option<&MachineName> = None;
        let mut preferred_answered = false;
        for candidate in candidates {
            let name = &candidate.name;
            let preferred = self.prefer.as_deref() == Some(name.as_str());
            let Some(reading) = &candidate.reading else {
                let _ = writeln!(
                    said,
                    "  {:<18} no runner for this dibs yet, so ranked last{}",
                    name.as_str(),
                    match preferred {
                        true => " but for its cache",
                        false => "",
                    }
                );
                preferred_answered |= preferred;
                unread = unread.or(Some(name));
                continue;
            };
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
        let placed = match (&self.prefer, cached, best, unread) {
            (Some(prefer), _, _, _) if preferred_answered => Ok(MachineName::new(prefer.as_str())),
            (_, Some((cached, _)), _, _) => Ok(cached.clone()),
            (_, None, Some((best, _)), _) => Ok(best.clone()),
            (_, None, None, Some(unread)) => Ok(unread.clone()),
            (_, None, None, None) => Err(match (&self.repo, candidates.is_empty()) {
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
        self.pick_where(|_| true)
    }

    /// As `pick`, among the machines `fits` accepts too.
    pub fn pick_where(&self, fits: impl Fn(&Machine) -> bool) -> Result<MachineName, Unplaced> {
        let call = self.machine.call;
        let mut names = self.machine.fleet.names();
        names.retain(|name| self.machine.fleet.entry(name.as_str()).is_none_or(&fits));
        if let Some(alias) = &call.device {
            names.retain(|name| {
                let carries = self
                    .machine
                    .fleet
                    .entry(name.as_str())
                    .is_some_and(|entry| Card::all_on(alias, entry).is_ok());
                if !carries && call.verbose {
                    eprintln!("  {:<18} has no {alias}", name.as_str());
                }
                carries
            });
            if names.is_empty() {
                return Err(Unplaced::NoCard {
                    alias: alias.clone(),
                });
            }
        }
        if let Some(inventory) = &self.machine.fleet.inventory
            && inventory.has_roles()
        {
            let role = Role::of_shared(call.device.as_ref());
            names.retain(|name| {
                let has = inventory
                    .machine(name.as_str())
                    .is_some_and(|m| m.roles.contains(&role));
                if !has && call.verbose {
                    eprintln!("  {:<18} not a {role} machine", name.as_str());
                }
                has
            });
            if names.is_empty() {
                return Err(Unplaced::NoRole { role });
            }
        }
        let candidates = self.candidates(&names);
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

    /// Every named machine asked at once; those that answered, and could start the work.
    fn candidates(&self, names: &[MachineName]) -> Vec<Candidate> {
        let call = self.machine.call;
        let down = RouteDown::from_env(self.machine.paths.route_down());
        let mut asked = Vec::new();
        for name in names {
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
            if answer.output.is_empty() && answer.exit == Some(i32::from(Exit::NoRunner.code())) {
                down.clear(&name);
                let entry = self.machine.fleet.entry(name.as_str());
                candidates.push(Candidate {
                    workstation: entry.is_some_and(|m| m.workstation),
                    measures: entry.is_none_or(|m| m.measure),
                    name,
                    reading: None,
                });
                continue;
            }
            if answer.output.is_empty() {
                if call.verbose {
                    eprintln!("  {:<18} no answer", name.as_str());
                }
                down.mark(&name);
                continue;
            }
            down.clear(&name);
            let Some(reading) = Reading::answered(&answer.output) else {
                continue;
            };
            if reading.closed.is_some()
                && std::env::var("DIBS_WHEN_OPEN")
                    .ok()
                    .is_none_or(|v| v != "1")
            {
                if call.verbose {
                    eprintln!("  {:<18} closed by its owners", name.as_str());
                }
                continue;
            }
            let entry = self.machine.fleet.entry(name.as_str());
            candidates.push(Candidate {
                workstation: entry.is_some_and(|m| m.workstation),
                measures: entry.is_none_or(|m| m.measure),
                name,
                reading: Some(reading),
            });
        }
        candidates
    }

    /// Where a measurement naming no machine goes, when the inventory gives roles: a `bench`
    /// machine that measures and has the cards it names. One where a label is being measured or
    /// waits to be wins, so a first run's series is not begun twice; then the one it was last
    /// measured on, by anyone; then the one with the fewest benchmarks running or waiting.
    /// `--new-series` skips the series, not the run being measured.
    pub fn pick_bench(&self, labels: &[Label]) -> Result<MachineName, Unplaced> {
        let fresh = match self.machine.call.new_series {
            true => labels.to_vec(),
            false => Vec::new(),
        };
        self.pick_bench_where(labels, &fresh, |_| true)
    }

    /// As `pick_bench`, among the machines `fits` accepts too, skipping the series of the labels
    /// in `fresh` alone.
    pub fn pick_bench_where(
        &self,
        labels: &[Label],
        fresh: &[Label],
        fits: impl Fn(&Machine) -> bool,
    ) -> Result<MachineName, Unplaced> {
        let call = self.machine.call;
        // As the runners and the series here file them.
        let labels: &[Label] = &labels.iter().map(Label::filed).collect::<Vec<_>>();
        let fresh: &[Label] = &fresh.iter().map(Label::filed).collect::<Vec<_>>();
        let Some(inventory) = self.machine.fleet.inventory.as_ref() else {
            return Err(Unplaced::NoRole { role: Role::Bench });
        };
        let names: Vec<MachineName> = inventory
            .machines
            .iter()
            .filter(|m| m.roles.contains(&Role::Bench) && m.measure && fits(m))
            .filter(|m| {
                let carries = call
                    .device
                    .as_ref()
                    .is_none_or(|alias| Card::all_on(alias, m).is_ok());
                if !carries && call.verbose {
                    eprintln!(
                        "  {:<18} has no {}",
                        m.name.as_str(),
                        call.device.as_ref().map(Alias::as_str).unwrap_or_default()
                    );
                }
                carries
            })
            .map(|m| m.name.clone())
            .collect();
        if names.is_empty() {
            return Err(match &call.device {
                Some(alias) => Unplaced::NoCard {
                    alias: alias.clone(),
                },
                None => Unplaced::NoRole { role: Role::Bench },
            });
        }
        let candidates = self.candidates(&names);
        let mine = self.machine.paths.series().map(|path| Series { path });
        let ours = |name: &MachineName| {
            let key = inventory
                .machine(name.as_str())
                .and_then(|m| m.series.clone().or(m.ssh.clone()))
                .unwrap_or_else(|| name.to_string());
            mine.as_ref().is_some_and(|s| {
                labels
                    .iter()
                    .filter(|l| !fresh.contains(l))
                    .any(|l| s.card_of(l.as_str(), &key).is_some())
            })
        };
        let measured = Measured { labels, fresh };
        // The jobs on the cards it would be given there, which a model's name does not match.
        let card_cost = |c: &Candidate| -> u64 {
            let (Some(alias), Some(r), Some(entry)) = (
                call.device.as_ref(),
                c.reading.as_ref(),
                self.machine.fleet.entry(c.name.as_str()),
            ) else {
                return 0;
            };
            let used = |card: &Alias| -> u64 {
                card.as_str()
                    .split(',')
                    .map(|one| r.using(one.trim()))
                    .sum()
            };
            Card::concrete(alias, entry, used).map_or(0, |chosen| used(&chosen))
        };
        let (chosen, others) = measured
            .choose(&candidates, ours, card_cost, coin)
            .ok_or_else(|| Unplaced::NoneAnswered {
                inventory: self.machine.fleet.shown(),
            })?;
        if !others.is_empty() {
            let others: Vec<&str> = others.iter().map(MachineName::as_str).collect();
            eprintln!(
                "dibs: {} is measured on {chosen}, where its series is newest; it also has one on {}",
                labels.first().map(Label::as_str).unwrap_or_default(),
                others.join(", ")
            );
        }
        if call.verbose {
            eprintln!("  measured on {chosen}");
        }
        Ok(chosen)
    }
}

/// How a measurement naming no machine chooses among the bench machines that answered.
struct Measured<'a> {
    labels: &'a [Label],
    /// The labels whose series are skipped, by `--new-series`.
    fresh: &'a [Label],
}

impl Measured<'_> {
    /// The machine, and the others the label also has a series on. One running or queueing the
    /// label wins, so two first runs do not begin two series; then the newest series, the
    /// runner's before this computer's own (`ours`); then the fewest benchmarks ahead.
    fn choose(
        &self,
        candidates: &[Candidate],
        ours: impl Fn(&MachineName) -> bool,
        card_cost: impl Fn(&Candidate) -> u64,
        mut coin: impl FnMut() -> bool,
    ) -> Option<(MachineName, Vec<MachineName>)> {
        let series_at = |c: &Candidate| -> Option<u64> {
            let reading = c.reading.as_ref();
            let running = self
                .labels
                .iter()
                .any(|l| reading.is_some_and(|r| r.measuring(l.as_str())));
            if running {
                return Some(u64::MAX);
            }
            let kept: Vec<&Label> = self
                .labels
                .iter()
                .filter(|l| !self.fresh.contains(l))
                .collect();
            kept.iter()
                .filter_map(|l| reading.and_then(|r| r.measured(l.as_str())))
                .max()
                .or((!kept.is_empty() && ours(&c.name)).then_some(0))
        };
        let series: Vec<(&MachineName, u64)> = candidates
            .iter()
            .filter_map(|c| series_at(c).map(|at| (&c.name, at)))
            .collect();
        if let Some(&(chosen, _)) = series.iter().max_by_key(|(_, at)| *at) {
            let others = series
                .iter()
                .filter(|(name, _)| *name != chosen)
                .map(|(name, _)| (*name).clone())
                .collect();
            return Some((chosen.clone(), others));
        }
        let mut best: Option<(&MachineName, u64)> = None;
        for candidate in candidates {
            let score = candidate.reading.as_ref().map_or(u64::MAX / 2, |r| {
                r.benchmarks() * 1000 + card_cost(candidate)
            });
            if best.is_none_or(|(_, b)| score < b || (score == b && coin())) {
                best = Some((&candidate.name, score));
            }
        }
        best.map(|(name, _)| (name.clone(), Vec::new()))
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
            now: Moment::epoch_now(),
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
        match self {
            Unplaced::NoCard { .. } | Unplaced::NoRole { .. } => Exit::Refused,
            Unplaced::NoClone { .. } | Unplaced::NoneAnswered { .. } => Exit::Unreachable,
        }
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
            Unplaced::NoCard { alias } => {
                writeln!(
                    f,
                    "dibs: no machine has a device called '{alias}', so there is nowhere to place this."
                )?;
                writeln!(
                    f,
                    "  dibs --machines -v lists each machine's cards by alias."
                )
            }
            Unplaced::NoRole { role } => {
                writeln!(
                    f,
                    "dibs: no machine that could take this is a {role} machine, so there is nowhere to place it."
                )?;
                writeln!(
                    f,
                    "  dibs --machines lists each machine's roles; --on <machine> names one."
                )
            }
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

    #[test]
    fn a_measurement_goes_where_its_label_runs_then_to_its_newest_series() {
        let bench = |label: &str| Occupant {
            mode: Some(Mode::Bench),
            device: None,
            label: label.into(),
        };
        let series = |label: &str, at| SeriesHere {
            label: label.into(),
            card: "none".into(),
            at,
        };
        let mut old = machine("old", LockState::Idle, 0);
        let mut new = machine("new", LockState::Idle, 0);
        let mut busy = machine("busy", LockState::Bench, 0);
        old.reading.as_mut().unwrap().series = vec![series("l", 10)];
        new.reading.as_mut().unwrap().series = vec![series("l", 20)];
        busy.reading.as_mut().unwrap().holders = vec![bench("other"), bench("other")];
        let labels = [Label::new("l")];
        let measured = |new_series: bool| Measured {
            labels: &labels,
            fresh: if new_series { &labels } else { &[] },
        };
        let all = [old.clone(), new.clone(), busy.clone()];
        let none = |_: &MachineName| false;
        assert_eq!(
            measured(false).choose(&all, none, |_| 0, || false),
            Some((MachineName::new("new"), vec![MachineName::new("old")])),
            "the newest series, naming the other"
        );
        let mut waiting = old.clone();
        waiting.reading.as_mut().unwrap().queue = vec![bench("l")];
        assert_eq!(
            measured(true)
                .choose(&[waiting, new.clone(), busy.clone()], none, |_| 0, || false)
                .map(|(m, _)| m),
            Some(MachineName::new("old")),
            "a first run where the label already waits joins it, --new-series or not"
        );
        assert_eq!(
            measured(true)
                .choose(&all, none, |_| 0, || false)
                .map(|(m, _)| m),
            Some(MachineName::new("old")),
            "--new-series skips the series and takes the fewest benchmarks ahead"
        );
        let fresh = [machine("a", LockState::Idle, 0), busy.clone()];
        let ours = |name: &MachineName| name.as_str() == "busy";
        assert_eq!(
            measured(false)
                .choose(&fresh, ours, |_| 0, || false)
                .map(|(m, _)| m),
            Some(MachineName::new("busy")),
            "a series only this computer kept still holds, behind any the runners keep"
        );
    }

    fn machine(name: &str, state: LockState, load: u64) -> Candidate {
        Candidate {
            name: MachineName::new(name),
            reading: Some(Reading {
                holders: Vec::new(),
                queue: Vec::new(),
                state,
                cores: Some(1),
                load: Some(load),
                caches: Some(Vec::new()),
                clones: Some(vec!["app".into()]),
                closed: None,
                series: Vec::new(),
            }),
            workstation: false,
            measures: true,
        }
    }

    #[test]
    fn a_machine_without_this_runner_is_placed_only_when_nothing_else_answered_or_for_its_cache() {
        let unread = |name: &str| Candidate {
            reading: None,
            ..machine(name, LockState::Idle, 0)
        };
        let placed = |ranking: &Ranking, candidates: &[Candidate]| {
            ranking.place(candidates, || false).placed.unwrap()
        };
        let any = Ranking::default();
        assert_eq!(
            placed(
                &any,
                &[unread("new"), machine("busy", LockState::Bench, 900)]
            )
            .as_str(),
            "busy"
        );
        assert_eq!(placed(&any, &[unread("new")]).as_str(), "new");
        let cache = Ranking {
            prefer: Some("new".into()),
            ..Ranking::default()
        };
        assert_eq!(
            placed(
                &cache,
                &[unread("new"), machine("idle", LockState::Idle, 0)]
            )
            .as_str(),
            "new"
        );
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
        cached.reading.as_mut().unwrap().caches = Some(vec!["app".into()]);
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
