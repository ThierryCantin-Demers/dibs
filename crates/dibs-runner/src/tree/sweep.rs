use crate::{
    clock::Moment,
    tree::{
        clocks::{Clocks, Fate, Removal, listed},
        git::Commands,
        runners::Runners,
    },
};
use std::path::Path;

/// What every prepare collects, of every repo rather than only the one being prepared: most runs
/// are local, and a repo nobody prepares any more would otherwise never be swept at all. The
/// current tree and target were touched a moment ago, and a running job touched its own when it
/// started, so neither can be a victim. `dibs --gc` judges by the same clocks.
pub struct Sweep<'a> {
    pub scratch: &'a Path,
    pub clocks: Clocks,
    /// The tree and target this prepare made.
    pub worktree: &'a Path,
    pub target: &'a Path,
    pub runners: &'a Runners,
    pub commands: &'a Commands<'a>,
    pub say: &'a dyn Fn(&str),
}

impl Sweep<'_> {
    pub fn run(&self) {
        let now = Moment::epoch_now();
        let removal = Removal {
            commands: Some(self.commands),
            say: self.say,
        };
        for repo in listed(&self.scratch.join("ws")) {
            for old in listed(&repo) {
                if old.is_dir() && old != self.worktree && self.clocks.tree(&old, now) == Fate::Past
                {
                    removal.tree(&old);
                }
            }
        }
        for old in listed(&self.scratch.join("jobs")) {
            if self.clocks.bulk(&old, now) {
                removal.path(&old);
            }
        }
        for old in listed(&self.scratch.join("target")) {
            if !old.is_dir() || old == self.target {
                continue;
            }
            match self.clocks.cache(&old, now) {
                Fate::Hollow => removal.hollow(&old),
                Fate::Past => {
                    removal.path(&old);
                }
                Fate::Kept | Fate::Dated | Fate::Held => {}
            }
        }
        self.runners.collect(&self.clocks, now, &removal);
    }
}
