//! Where a batch's rows go before any runs. Rows of one repo share its tree on a machine, and a
//! later one may read what an earlier one left there, so rows of one repo joined by `after=` are
//! one group, placed on the machine its most demanding row needs.

use super::{
    base::machine_of,
    parse::{BatchError, Step, StepKind},
};
use crate::{
    call::{CallError, MachineCall, Planned},
    caller::Caller,
    cli::RecipeVerb,
    cli::{Call, Mode},
    execution::{Job, Repo},
    git::Git,
    inventory::{Inventory, Machine},
    machine::Card,
    placement::Placement,
    recipe::{Checkouts, run_label},
};
use dibs_format::{Alias, Label, MachineName, Mode as JobMode};
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
        for group in self.groups(&measures) {
            if group.iter().all(|&i| named[i].is_some()) {
                continue;
            }
            // A row's own --on takes the group with it, read as the shell will expand it; one
            // that cannot be read leaves the group to place itself row by row.
            let pinned = group
                .iter()
                .find_map(|&i| self.steps[i].on.as_deref().map(expanded));
            let machine = match pinned {
                Some(pinned) => pinned,
                None => self.demand(&group, &measures)?,
            };
            for &i in &group {
                if named[i].is_none() {
                    placed[i].clone_from(&machine);
                }
            }
        }
        Ok(placed)
    }

    /// The one machine of a batch detached to it, which every row must run on: the one its
    /// rows name, or else, once the inventory gives roles, the one its most demanding row needs.
    pub fn one(&self) -> Result<Option<String>, BatchError> {
        if !by_roles() {
            let named: Vec<Option<String>> = self
                .steps
                .iter()
                .map(|s| machine_of(s, self.on).filter(|m| m != "?"))
                .collect();
            let first = named.iter().flatten().next().cloned();
            return Ok(first.filter(|_| named.iter().all(Option::is_some)));
        }
        let batch_on = self
            .on
            .map(MachineName::to_string)
            .or_else(|| std::env::var("DIBS_ON").ok().filter(|on| !on.is_empty()));
        if let Some(on) = batch_on {
            return Ok(Some(on));
        }
        if let Some(pinned) = self
            .steps
            .iter()
            .find_map(|s| s.on.as_deref().map(expanded))
        {
            return Ok(pinned);
        }
        let measures: Vec<bool> = self.steps.iter().map(|s| self.measures(s)).collect();
        let all: Vec<usize> = (0..self.steps.len()).collect();
        self.demand(&all, &measures)
    }

    /// Whether a row measures: a recipe by the locks of the jobs it resolves to, any other by
    /// what its line says.
    pub fn measures(&self, step: &Step) -> bool {
        if let Some(with) = step.recipe.as_ref().filter(|r| r.verb == RecipeVerb::With) {
            return with.bench;
        }
        match step.recipe.as_ref().and_then(Job::of_batch_line) {
            Some(jobs) => jobs.iter().any(|p| p.mode == Planned::Job(JobMode::Bench)),
            None => step.measures(),
        }
    }

    /// The repo a row works in, as a recipe resolves its checkout, or None for one that is not
    /// placed with the others.
    fn repo(&self, step: &Step) -> Option<String> {
        if step.lock == StepKind::Peek {
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
            if !joined[i] && !measures[i] {
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
        let card = |s: &Step| s.device.as_deref().map(|d| expanded(d).map(Alias::new));
        let mut cards: Vec<Alias> = Vec::new();
        for row in &rows {
            match card(row) {
                Some(Some(alias)) => cards.push(alias),
                Some(None) => return Ok(None),
                None => {}
            }
        }
        let measuring = group.iter().any(|&i| measures[i]);
        // The card the call is placed by is its measurement's; every other row's must be on the
        // machine as well, each as it asks.
        let lead = group
            .iter()
            .filter(|&&i| !measuring || measures[i])
            .find_map(|&i| card(&self.steps[i]).flatten());
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
            let Some(measured) = self.measured_labels(step) else {
                return Ok(None);
            };
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
            let label = run_label(
                &repo,
                "with",
                with.recipe.as_deref(),
                with.device.as_deref(),
            );
            return Some(vec![Label::new(expanded(&label)?)]);
        }
        let jobs = step.recipe.as_ref().and_then(Job::of_batch_line);
        match jobs {
            Some(jobs) => Some(
                jobs.into_iter()
                    .filter(|p| p.mode == Planned::Job(JobMode::Bench))
                    .map(|p| p.label)
                    .collect(),
            ),
            None => {
                let label = step.pending(true, self.cwd).label;
                Some(vec![Label::new(expanded(label.as_str())?)])
            }
        }
    }
}

/// A card as the step's shell will see it: `$NAME` and `${NAME}` from the batch's environment,
/// which its steps inherit. None for anything else the shell would expand.
fn expanded(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = word;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let named = |name: &str| name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        match after.strip_prefix('{') {
            Some(braced) => {
                let end = braced.find('}')?;
                let inner = &braced[..end];
                let value = match (inner.split_once(":-"), inner.split_once('-')) {
                    (Some((name, fallback)), _) if named(name) => std::env::var(name)
                        .ok()
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| fallback.to_string()),
                    (None, Some((name, fallback))) if named(name) => {
                        std::env::var(name).unwrap_or_else(|_| fallback.to_string())
                    }
                    _ if !inner.is_empty() && named(inner) => std::env::var(inner).ok()?,
                    _ => return None,
                };
                out.push_str(&value);
                rest = &braced[end + 1..];
            }
            None => {
                let end = after
                    .find(|c: char| !named(&c.to_string()))
                    .unwrap_or(after.len());
                if end == 0 {
                    return None;
                }
                out.push_str(&std::env::var(&after[..end]).ok()?);
                rest = &after[end..];
            }
        }
    }
    out.push_str(rest);
    let unread = ['"', '\'', '`', '*', '?', '$'];
    (!out.contains(unread)).then_some(out)
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
        unsafe { std::env::set_var("DIBS_PLACE_CARD", "gpu:x") };
        assert_eq!(expanded("$DIBS_PLACE_CARD").as_deref(), Some("gpu:x"));
        assert_eq!(expanded("${DIBS_PLACE_CARD}.a").as_deref(), Some("gpu:x.a"));
        assert_eq!(
            expanded("${DIBS_PLACE_UNSET:-gpu:y}").as_deref(),
            Some("gpu:y")
        );
        assert_eq!(
            expanded("${DIBS_PLACE_UNSET-gpu:z}").as_deref(),
            Some("gpu:z")
        );
        assert_eq!(expanded("$DIBS_PLACE_UNSET"), None);
        assert_eq!(expanded("${DIBS_PLACE_CARD/x/y}"), None);
        assert_eq!(expanded("gpu:*"), None);
    }
}
