use super::{
    pins::{PinSpec, PinnedRepo},
    refs::Side,
};
use crate::{
    call::{Pending, Planned},
    cli::RecipeCall,
    recipe::{self, Lock, Resolved},
};
use dibs_format::Mode;

/// One job of a recipe run, in the order they are sent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Job {
    /// The local tree sent and prepared, in one transfer.
    Send(usize),
    /// A fetched tree prepared on its own, since the step after it may not carry it.
    Setup(usize),
    /// A step of an arm, carrying the arm's setup at its head when `setup`. `rep` is None for
    /// the steps that run once, ahead of what is repeated.
    Step {
        arm: usize,
        step: usize,
        rep: Option<u32>,
        setup: bool,
    },
}

impl Job {
    /// Every arm is built before any is measured, then each rep measures them all, the order
    /// reversed every other rep: A B B A cancels a drift linear in time, such as a card warming
    /// up, which A B A B credits to B. Without an exclusive step the whole recipe repeats.
    pub fn schedule(local: &[bool], steps: &[recipe::Step], reps: u32) -> Vec<Job> {
        let first = steps
            .iter()
            .position(|s| s.lock == Lock::Exclusive)
            .unwrap_or(0);
        let mut ready = vec![false; local.len()];
        let mut jobs = Vec::new();
        let mut visit = |arm: usize, step: usize, rep: Option<u32>| {
            let fresh = !std::mem::replace(&mut ready[arm], true);
            let fold = fresh && !local[arm] && steps[step].lock == Lock::Shared;
            match (fresh, local[arm]) {
                (true, true) => jobs.push(Job::Send(arm)),
                (true, false) if !fold => jobs.push(Job::Setup(arm)),
                _ => {}
            }
            jobs.push(Job::Step {
                arm,
                step,
                rep,
                setup: fold,
            });
        };
        for arm in 0..local.len() {
            for step in 0..first {
                visit(arm, step, None);
            }
        }
        for rep in 1..=reps {
            let order: Vec<usize> = match rep % 2 {
                1 => (0..local.len()).collect(),
                _ => (0..local.len()).rev().collect(),
            };
            for arm in order {
                for step in first..steps.len() {
                    visit(arm, step, Some(rep));
                }
            }
        }
        jobs
    }

    /// The jobs a recipe makes, in the order `Job::schedule` sends them, under the labels their
    /// durations are filed by. The setup rides at the head of the first job that needs the tree:
    /// the transfer for a local tree, or the first step when it is shared. Its own job would be a
    /// second round trip and a second place in the queue. An exclusive first step keeps its setup
    /// apart, or a fetch would run inside the hold.
    pub fn pending(
        r: &Resolved,
        sides: &[Side],
        local: &[bool],
        reps: u32,
        pins: &[PinnedRepo],
    ) -> Vec<Pending> {
        let of = |arm: usize, rep: Option<u32>| {
            let mut tags = Vec::new();
            if sides.len() > 1 {
                tags.push(sides[arm].name());
            }
            if let Some(r) = rep.filter(|_| reps > 1) {
                tags.push(format!("r{r}"));
            }
            match tags.is_empty() {
                true => String::new(),
                false => format!(" ({})", tags.join(" ")),
            }
        };
        let job = |label: String, mode: Mode, tag: String| Pending {
            name: format!("{label}{tag}"),
            mode: Planned::Job(mode),
            label,
            here: true,
        };
        let pinned = pins.iter().map(|pin| {
            job(
                format!("{}:pin", r.label),
                if pin.local { Mode::Rsh } else { Mode::Shared },
                format!(" ({})", pin.repo),
            )
        });
        pinned
            .chain(
                Job::schedule(local, &r.rec.steps, reps)
                    .into_iter()
                    .map(|j| match j {
                        Job::Send(a) => job(format!("{}:send", r.label), Mode::Rsh, of(a, None)),
                        Job::Setup(a) => {
                            job(format!("{}:setup", r.label), Mode::Shared, of(a, None))
                        }
                        Job::Step { arm, step, rep, .. } => {
                            let mode = match r.rec.steps[step].lock {
                                Lock::Shared => Mode::Shared,
                                Lock::Exclusive => Mode::Bench,
                            };
                            job(r.step_labels[step].clone(), mode, of(arm, rep))
                        }
                    }),
            )
            .collect()
    }

    /// The jobs a recipe line in a batch will make, so the batch's plan can estimate them. None
    /// when the line does not resolve here, which leaves that step without an estimate.
    pub fn of_batch_line(args: &RecipeCall) -> Option<Vec<Pending>> {
        let r = Resolved::of(args).ok()?;
        let pins = args
            .pins
            .iter()
            .map(|p| {
                PinSpec::parse(p).map(|s| PinnedRepo {
                    repo: s.repo.to_string(),
                    local: s.reference == "local",
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        // Whether a ref is sent is only known once it is looked up, so a ref is planned as fetched.
        let sides = Side::list(args.reference.as_deref()).ok()?;
        let local: Vec<bool> = sides.iter().map(Side::sent).collect();
        Some(Job::pending(&r, &sides, &local, args.reps, &pins))
    }
}
