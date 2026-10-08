use crate::{
    history::{History, Key, Scope},
    queue::Eta,
};
use dibs_format::{
    BatchPlan, Mode, PendingKind, Span,
    status::{BatchShown, Left},
};

/// Steps named in full; the rest are counted.
const NAMED: usize = 6;

/// Where the queue stands once everything waiting now has started, which a batch's steps still to
/// come arrive behind.
#[derive(Debug, Clone, Copy)]
pub struct Tail {
    pub eta: Eta,
    /// A benchmark holds or waits, so a shared step waits for the queue too.
    pub exclusive: bool,
}

/// A batch's plan, read for one of its jobs.
pub struct Plan<'a> {
    pub plan: BatchPlan,
    pub history: &'a History,
}

impl Plan<'_> {
    /// What the batch still has to do, and how long that takes here once this job's own `left`
    /// has passed. Only a label's own history counts: what an agent usually takes says nothing
    /// of a step that has never run. Where the queue cannot be estimated a step is placed as if
    /// it were empty, and the total becomes a floor.
    pub fn shown(self, left: Option<u64>, tail: &Tail) -> BatchShown {
        let mut eta = tail.eta;
        let mut left = left;
        let mut partial = false;
        let mut here = 0;
        let mut next = Vec::new();
        let mut far = Vec::new();
        for step in &self.plan.pending {
            if !step.here {
                far.push(step.name.clone());
                continue;
            }
            here += 1;
            let mode = match step.kind {
                PendingKind::Job(Mode::Peek) => {
                    next.push(step.name.clone());
                    continue;
                }
                PendingKind::Job(mode) => Some(mode),
                PendingKind::Recipe => None,
            };
            let bench = mode == Some(Mode::Bench);
            let mut start = left;
            if let Some(after) = left {
                if bench {
                    match eta.known && eta.pending_known {
                        true => start = Some(after.max(eta.at + eta.pending)),
                        false => partial = true,
                    }
                } else if tail.exclusive {
                    match eta.known {
                        true => start = Some(after.max(eta.at)),
                        false => partial = true,
                    }
                }
            }
            let estimate = mode
                .and_then(|mode| {
                    self.history.estimate(Key {
                        mode,
                        label: &step.label,
                        agent: None,
                        fingerprint: None,
                    })
                })
                .filter(|e| e.scope == Scope::This);
            let said = match estimate {
                Some(estimate) => {
                    if let Some(start) = start {
                        let ends = start + estimate.median;
                        left = Some(ends);
                        if bench {
                            eta = Eta {
                                at: ends,
                                known: true,
                                pending: 0,
                                pending_known: true,
                            };
                        } else if ends.saturating_sub(eta.at) > eta.pending {
                            eta.pending = ends - eta.at;
                        }
                    }
                    format!(" ~{}", Span(estimate.median))
                }
                None => {
                    partial = true;
                    left = start;
                    " (no history)".to_string()
                }
            };
            next.push(format!("{}{said}", step.name));
        }
        let elsewhere = far.len();
        BatchShown {
            id: self.plan.batch,
            step: self.plan.step,
            k: self.plan.position,
            n: self.plan.total,
            here,
            elsewhere,
            next: named(next),
            far: named(far),
            left: left.map(|left| Left {
                left,
                left_partial: partial,
            }),
        }
    }
}

/// The first few by name, then how many more.
fn named(names: Vec<String>) -> String {
    let more = names.len().saturating_sub(NAMED);
    let mut said = names.into_iter().take(NAMED).collect::<Vec<_>>().join(", ");
    if more > 0 {
        said.push_str(&format!(" and {more} more"));
    }
    said
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn past_six_steps_the_rest_are_counted() {
        let names: Vec<String> = (1..=8).map(|n| format!("s{n}")).collect();
        assert_eq!(named(names), "s1, s2, s3, s4, s5, s6 and 2 more");
        assert_eq!(named(vec!["a".into()]), "a");
    }
}
