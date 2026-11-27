//! Where a batch's rows go before any runs. Rows of one repo share its tree on a machine, and a
//! later one may read what an earlier one left there, so rows of one repo joined by `after=` are
//! one group, placed on the machine its most demanding row needs.

use super::{
    base::machine_of,
    parse::{BatchError, Step, StepKind},
};
use crate::{
    call::{CallError, MachineCall},
    caller::Caller,
    cli::{Call, Mode},
    cli::{RecipeCall, RecipeVerb},
    execution::{Job, Repo},
    git::Git,
    inventory::{Inventory, Machine},
    machine::Card,
    placement::Placement,
    recipe::{Checkouts, run_label},
};
use dibs_format::{Alias, Label, MachineName, Mode as JobMode, PendingStepKind};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub struct Rows<'a> {
    pub steps: &'a [Step],
    pub on: Option<&'a MachineName>,
    pub cwd: &'a str,
}

impl Rows<'_> {
    /// The machine of each row naming none that goes with its group, given `named`, what each
    /// row names. None for a row left to place itself as it starts, which is a shared row no row
    /// of its repo waits for or is waited for by, for a measurement nowhere to go, and for every
    /// row of an inventory that gives no roles, which places as it always has.
    pub fn place(&self, named: &[Option<String>]) -> Result<Vec<Option<String>>, BatchError> {
        let mut placed: Vec<Option<String>> = vec![None; self.steps.len()];
        if !by_roles() {
            return Ok(placed);
        }
        let measures: Vec<bool> = self.steps.iter().map(|s| self.measures(s)).collect();
        let known = |i: usize| named[i].as_deref().is_some_and(|m| m != "?");
        for group in self.groups(&measures) {
            if group.iter().all(|&i| known(i)) {
                continue;
            }
            // A row's own --on takes the group with it, read as the shell will expand it.
            let pinned = group.iter().find_map(|&i| self.pinned(i).transpose());
            let machine = match pinned {
                Some(pinned) => Some(pinned?),
                None => self.demand(&group, &measures)?,
            };
            for &i in &group {
                if !known(i) {
                    placed[i].clone_from(&machine);
                }
            }
        }
        Ok(placed)
    }

    /// The one machine of a batch detached to it, which every row must run on: the one its
    /// rows name, or else, once the inventory gives roles, the one its most demanding row needs.
    pub fn one(&self) -> Result<Option<String>, BatchError> {
        // A query such as --machines runs wherever the batch is driven, so it places nothing.
        let running: Vec<usize> = (0..self.steps.len())
            .filter(|&i| self.steps[i].runs)
            .collect();
        if !by_roles() || running.is_empty() {
            let named: Vec<Option<String>> = self
                .steps
                .iter()
                .map(|s| machine_of(s, self.on).filter(|m| m != "?"))
                .collect();
            let first = named.iter().flatten().next().cloned();
            return Ok(first.filter(|_| named.iter().all(Option::is_some)));
        }
        if let Some(pinned) = (0..self.steps.len()).find_map(|i| self.pinned(i).transpose()) {
            return pinned.map(Some);
        }
        if let Some(on) = self.batch_on() {
            return Ok(Some(on));
        }
        let measures: Vec<bool> = self.steps.iter().map(|s| self.measures(s)).collect();
        self.demand(&running, &measures)
    }

    /// A row's own --on as the shell will expand it: None when it names none, Some(None) when
    /// it cannot be read here.
    pub fn pin(&self, step: &Step) -> Option<Option<String>> {
        step.on.as_deref().map(|on| word(step, on))
    }

    /// Row `i`'s own --on, refused when it cannot be read here.
    fn pinned(&self, i: usize) -> Result<Option<String>, BatchError> {
        let step = &self.steps[i];
        match self.pin(step) {
            Some(Some(on)) => Ok(Some(on)),
            Some(None) => Err(unread(step, step.on.as_deref())),
            None => Ok(None),
        }
    }

    /// The machine of every row naming none of its own, from `--on` or `DIBS_ON`.
    pub fn batch_on(&self) -> Option<String> {
        self.on
            .map(MachineName::to_string)
            .or_else(|| std::env::var("DIBS_ON").ok().filter(|on| !on.is_empty()))
    }

    /// Whether a row measures: a recipe by the locks of the jobs it resolves to, any other by
    /// what its line says.
    pub fn measures(&self, step: &Step) -> bool {
        if let Some(with) = step.recipe.as_ref().filter(|r| r.verb == RecipeVerb::With) {
            return with.bench;
        }
        match step.recipe.as_ref().and_then(Job::of_batch_line) {
            Some(jobs) => jobs
                .iter()
                .any(|p| p.kind == PendingStepKind::Job(JobMode::Bench)),
            None => step.measures(),
        }
    }

    /// The repo a row works in, as a recipe resolves its checkout, or None for one that is not
    /// placed with the others.
    fn repo(&self, step: &Step) -> Option<String> {
        if step.lock == StepKind::Peek || !step.runs {
            return None;
        }
        let checkout = match &step.recipe {
            Some(recipe) => Checkouts::of(recipe)
                .and_then(|c| c.find(&recipe.repo))
                .ok(),
            None => Git(Path::new(self.cwd))
                .run(&["rev-parse", "--show-toplevel"])
                .ok()
                .map(|top| PathBuf::from(top.trim())),
        };
        Some(match checkout {
            Some(dir) => Repo(&dir).identity(),
            None => match &step.recipe {
                Some(recipe) if recipe.repo != "." => recipe.repo.clone(),
                _ => self.cwd.rsplit('/').next().unwrap_or_default().to_string(),
            },
        })
    }

    /// Rows joined by `after=` to a row of the same repo, in either direction, with a lone
    /// measurement as a group of its own since it must be placed too.
    fn groups(&self, measures: &[bool]) -> Vec<Vec<usize>> {
        let index: HashMap<&str, usize> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.as_str(), i))
            .collect();
        let repos: Vec<Option<String>> = self.steps.iter().map(|s| self.repo(s)).collect();
        let mut parent: Vec<usize> = (0..self.steps.len()).collect();
        fn root(parent: &mut [usize], i: usize) -> usize {
            let mut i = i;
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let mut joined = vec![false; self.steps.len()];
        for (i, step) in self.steps.iter().enumerate() {
            for after in &step.after {
                let j = index[after.as_str()];
                if repos[i].is_some() && repos[i] == repos[j] {
                    let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                    parent[a] = b;
                    joined[i] = true;
                    joined[j] = true;
                }
            }
        }
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut at: HashMap<usize, usize> = HashMap::new();
        for i in 0..self.steps.len() {
            // A lone row is placed too, so the scheduler sees the machine it shares with a
            // placed group rather than `?`.
            let alone = matches!(self.steps[i].lock, StepKind::Peek | StepKind::Sync);
            if !joined[i] && !measures[i] && (!self.steps[i].runs || alone) {
                continue;
            }
            let r = root(&mut parent, i);
            let g = *at.entry(r).or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
            groups[g].push(i);
        }
        groups
    }

    /// The machine a group naming none goes to: a bench machine by its measured labels when any
    /// row measures, else where its shared work would go. The machine must carry every card any
    /// row names, since every row runs there.
    fn demand(&self, group: &[usize], measures: &[bool]) -> Result<Option<String>, BatchError> {
        let caller = Caller::default();
        let rows: Vec<&Step> = group.iter().map(|&i| &self.steps[i]).collect();
        let card = |s: &Step| s.device.as_deref().map(|d| word(s, d).map(Alias::new));
        let mut cards: Vec<Alias> = Vec::new();
        for row in &rows {
            match card(row) {
                Some(Some(alias)) => cards.push(alias),
                Some(None) => return Err(unread(row, row.device.as_deref())),
                None => {}
            }
        }
        let measuring = group.iter().any(|&i| measures[i]);
        // The card the call is placed by is its measurement's; every other row's must be on the
        // machine as well, each as it asks.
        // Shared work needing a card goes where that card is, whichever row names it.
        let leading = |i: &&usize| !measuring || measures[**i];
        let named_card = |i: &usize| card(&self.steps[*i]).flatten();
        let lead = group
            .iter()
            .filter(leading)
            .filter_map(named_card)
            .find(|alias| alias.as_str() != "cpu")
            .or_else(|| group.iter().filter(leading).find_map(named_card));
        let fits = |m: &Machine| cards.iter().all(|alias| Card::all_on(alias, m).is_ok());
        let call = Call {
            mode: Mode::Pick,
            device: lead,
            // Only a recipe is placed by where its repo is cloned; a raw row is not.
            repo: rows
                .iter()
                .find(|s| s.recipe.as_ref().is_some_and(|r| r.verb != RecipeVerb::Raw))
                .and_then(|s| self.repo(s)),
            ..Call::default()
        };
        let machine = MachineCall::new(&call, &caller)?;
        let placement = Placement { machine: &machine };
        if !measuring {
            return Ok(Some(
                placement
                    .pick_where(fits)
                    .map_err(CallError::from)?
                    .to_string(),
            ));
        }
        let mut labels = Vec::new();
        let mut fresh = Vec::new();
        let mut kept = Vec::new();
        for &i in group.iter().filter(|&&i| measures[i]) {
            let step = &self.steps[i];
            // A label the step gets only as it starts is placed without its series rather than
            // apart from the rows it reads from.
            let measured = self.measured_labels(step).unwrap_or_default();
            match step.new_series {
                true => fresh.extend(measured.iter().cloned()),
                false => kept.extend(measured.iter().cloned()),
            }
            labels.extend(measured);
        }
        // A label one row keeps and another starts again keeps its series until it starts again.
        fresh.retain(|l| !kept.contains(l));
        Ok(Some(
            placement
                .pick_bench_where(&labels, &fresh, fits)
                .map_err(CallError::from)?
                .to_string(),
        ))
    }

    /// The labels a measuring row will send, None when the shell expands one in a way this
    /// cannot read.
    fn measured_labels(&self, step: &Step) -> Option<Vec<Label>> {
        if let Some(with) = step.recipe.as_ref().filter(|r| r.verb == RecipeVerb::With) {
            let repo = self.repo(step).unwrap_or_else(|| with.repo.clone());
            let device = match with.device.as_deref() {
                Some(d) => Some(word(step, d)?),
                None => None,
            };
            let label = run_label(&repo, "with", with.recipe.as_deref(), device.as_deref());
            return Some(vec![Label::new(label)]);
        }
        let jobs = match step.recipe.as_ref() {
            Some(r) => {
                let device = match r.device.as_deref() {
                    Some(d) => Some(word(step, d)?),
                    None => None,
                };
                Job::of_batch_line(&RecipeCall {
                    device,
                    ..r.clone()
                })
            }
            None => None,
        };
        match jobs {
            Some(jobs) => Some(
                jobs.into_iter()
                    .filter(|p| p.kind == PendingStepKind::Job(JobMode::Bench))
                    .map(|p| p.label)
                    .collect(),
            ),
            None => {
                let label = step.pending(true, self.cwd).label;
                Some(vec![Label::new(word(step, label.as_str())?)])
            }
        }
    }
}

/// A row's word as its shell will expand it, None when a `$` in it is quoted and not a variable.
fn word(step: &Step, word: &str) -> Option<String> {
    match step.literal && word.contains('$') {
        true => None,
        false => expanded(word),
    }
}

/// A word as the step's shell will expand it from the batch's environment, which its steps
/// inherit: `$X`, `${X}`, `${X:-y}`, `${X-y}`, `${X,,}` and `${X^^}`. None for anything else, and
/// for a word that expands to nothing; it is read here and never handed to a shell.
fn expanded(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = word;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (value, tail) = match after.strip_prefix('{') {
            Some(braced) => {
                let end = braced.find('}')?;
                (parameter(&braced[..end])?, &braced[end + 1..])
            }
            None => {
                let end = name_len(after);
                if end == 0 {
                    return None;
                }
                (std::env::var(&after[..end]).ok()?, &after[end..])
            }
        };
        out.push_str(&value);
        rest = tail;
    }
    out.push_str(rest);
    (!out.is_empty()).then_some(out)
}

fn name_len(s: &str) -> usize {
    s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len())
}

/// The inside of a `${...}`.
fn parameter(inner: &str) -> Option<String> {
    let (name, op) = inner.split_at(name_len(inner));
    if name.is_empty() {
        return None;
    }
    let value = std::env::var(name).ok();
    let fallback = |f: &str| (!f.contains('$')).then(|| f.to_string());
    match op {
        "" => value,
        ",," => value.map(|v| v.to_lowercase()),
        "^^" => value.map(|v| v.to_uppercase()),
        _ => match (op.strip_prefix(":-"), op.strip_prefix('-')) {
            (Some(f), _) => value.filter(|v| !v.is_empty()).or_else(|| fallback(f)),
            (None, Some(f)) => value.or_else(|| fallback(f)),
            _ => None,
        },
    }
}

fn unread(step: &Step, word: Option<&str>) -> BatchError {
    BatchError::Unread {
        step: step.name.clone(),
        word: word.unwrap_or_default().to_string(),
    }
}

pub(super) fn by_roles() -> bool {
    Inventory::here()
        .ok()
        .flatten()
        .is_some_and(|i| i.has_roles())
}

#[cfg(test)]
mod tests {
    use super::expanded;

    #[test]
    fn a_word_is_read_as_the_shell_would_expand_it() {
        // SAFETY: set once, read by this test alone.
        unsafe { std::env::set_var("DIBS_PLACE_CARD", "GPU:x") };
        assert_eq!(expanded("gpu:x").as_deref(), Some("gpu:x"));
        assert_eq!(
            expanded("${DIBS_PLACE_CARD,,}.a").as_deref(),
            Some("gpu:x.a")
        );
        assert_eq!(
            expanded("${DIBS_PLACE_UNSET:-gpu:y}").as_deref(),
            Some("gpu:y")
        );
        assert_eq!(expanded("$DIBS_PLACE_UNSET"), None, "nothing is no card");
        assert_eq!(
            expanded("$(echo gpu:x)"),
            None,
            "a command is not run to plan"
        );
        assert_eq!(expanded("${DIBS_PLACE_CARD@P}"), None);
        assert_eq!(expanded("${DIBS_PLACE_UNSET:-$HOME}"), None);
    }
}
