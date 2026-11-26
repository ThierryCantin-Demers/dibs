//! Where a batch's rows go before any runs. Rows of one repo share its tree on a machine, and a
//! later one may read what an earlier one left there, so rows of one repo joined by `after=` are
//! one group, placed on the machine its most demanding row needs.

use super::{
    base::machine_of,
    parse::{BatchError, Step, StepKind},
};
use crate::{
    call::{MachineCall, Planned},
    caller::Caller,
    cli::{Call, Mode},
    execution::Job,
    inventory::Inventory,
    placement::Placement,
};
use dibs_format::{Alias, Label, MachineName, Mode as JobMode};
use std::collections::HashMap;

pub struct Rows<'a> {
    pub steps: &'a [Step],
    pub on: Option<&'a MachineName>,
    pub cwd: &'a str,
}

impl Rows<'_> {
    /// The machine of each row naming none that goes with its group, given `named`, what each
    /// row names. None for a row left to place itself as it starts, which is a shared row no row
    /// of its repo waits for or is waited for by, and for a measurement nowhere to go.
    pub fn place(&self, named: &[Option<String>]) -> Result<Vec<Option<String>>, BatchError> {
        let mut placed: Vec<Option<String>> = vec![None; self.steps.len()];
        for group in self.groups() {
            let chosen = group
                .iter()
                .find_map(|&i| named[i].clone().filter(|m| m != "?"));
            if group.iter().all(|&i| named[i].is_some()) {
                continue;
            }
            let machine = match chosen {
                Some(chosen) => Some(chosen),
                None => self.demand(&group)?,
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
    /// rows name, or else the one its most demanding row needs.
    pub fn one(&self) -> Result<Option<String>, BatchError> {
        let named = self
            .steps
            .iter()
            .find_map(|s| machine_of(s, self.on).filter(|m| m != "?"));
        match named {
            Some(named) => Ok(Some(named)),
            None => self.demand(&(0..self.steps.len()).collect::<Vec<_>>()),
        }
    }

    /// The repo a row works in, or None for one that is not placed with the others.
    fn repo(&self, step: &Step) -> Option<String> {
        let here = || self.cwd.rsplit('/').next().unwrap_or_default().to_string();
        match (&step.lock, &step.recipe) {
            (StepKind::Peek, _) => None,
            (_, Some(recipe)) if recipe.repo != "." => Some(recipe.repo.clone()),
            _ => Some(here()),
        }
    }

    /// Rows joined by `after=` to a row of the same repo, in either direction, with a lone
    /// measurement as a group of its own since it must be placed too.
    fn groups(&self) -> Vec<Vec<usize>> {
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
            if !joined[i] && !self.steps[i].measures() {
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

    /// The machine a group naming none goes to. A group that measures goes to a bench machine
    /// by its measured labels, once the inventory gives roles, and is otherwise left unplaced
    /// for the caller to refuse; any other goes where its shared work would.
    fn demand(&self, group: &[usize]) -> Result<Option<String>, BatchError> {
        let caller = Caller::default();
        let rows: Vec<&Step> = group.iter().map(|&i| &self.steps[i]).collect();
        let card = |measuring: bool| {
            rows.iter()
                .filter(|s| !measuring || s.measures())
                .find_map(|s| s.device.clone().filter(|d| d != "cpu"))
                .map(|d| Alias::new(&d))
        };
        let measures = rows.iter().any(|s| s.measures());
        let call = Call {
            mode: Mode::Pick,
            device: card(measures),
            new_series: rows
                .iter()
                .any(|s| s.recipe.as_ref().is_some_and(|r| r.new_series)),
            repo: rows.iter().find_map(|s| self.repo(s)),
            ..Call::default()
        };
        let machine = MachineCall::new(&call, &caller)?;
        let placement = Placement { machine: &machine };
        if !measures {
            // A shared group nowhere answers for is left to place itself, as a lone row is.
            return Ok(placement.pick().ok().map(|m| m.to_string()));
        }
        if !machine
            .fleet
            .inventory
            .as_ref()
            .is_some_and(Inventory::has_roles)
        {
            return Ok(None);
        }
        let labels: Vec<Label> = rows
            .iter()
            .filter(|s| s.measures())
            .flat_map(|s| self.measured_labels(s))
            .collect();
        Ok(Some(
            placement
                .pick_bench(&labels)
                .map_err(CallErrorOf::from)?
                .to_string(),
        ))
    }

    fn measured_labels(&self, step: &Step) -> Vec<Label> {
        let jobs = step.recipe.as_ref().and_then(Job::of_batch_line);
        match jobs {
            Some(jobs) => jobs
                .into_iter()
                .filter(|p| p.mode == Planned::Job(JobMode::Bench))
                .map(|p| p.label)
                .collect(),
            None => vec![step.pending(true, self.cwd).label.filed()],
        }
    }
}

type CallErrorOf = crate::call::CallError;
