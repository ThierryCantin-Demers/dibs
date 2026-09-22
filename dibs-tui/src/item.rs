//! One row of the jobs table, and what the pane under it says about that job.

use crate::{
    status::{Batch, Holder, Queued, Status},
    text::dur,
};

#[derive(Clone)]
pub struct Item {
    pub machine: String,
    /// Which card the job was pinned to, where one was named. Absent is the common case and
    /// is not a fault: a build does not want a card.
    pub device: Option<String>,
    pub holding: bool,
    pub slot: String,
    pub mode: String,
    pub pid: i64,
    pub label: String,
    pub agent: String,
    pub cmd: String,
    pub time: i64,
    pub cpu: Option<i64>,
    pub rate: Option<i64>,
    pub eta: Option<i64>,
    pub note: String,
    /// The column is narrow and the pane is not, so each says as much as it has room for.
    pub long: String,
    pub alarm: bool,
    pub output: Option<String>,
    pub batch: Option<Batch>,
}

impl Item {
    /// Every job one machine's document lists, holders before the queue.
    pub fn all(machine: &str, s: &Status) -> Vec<Item> {
        s.holders
            .iter()
            .map(|h| Item::holding(machine, h))
            .chain(s.queue.iter().map(|q| Item::queued(machine, q)))
            .map(Item::with_batch_step)
            .collect()
    }

    fn holding(machine: &str, h: &Holder) -> Item {
        let Verdict { note, long, alarm } = Verdict::of_holder(h);
        Item {
            machine: machine.to_string(),
            device: pinned(&h.device),
            holding: true,
            slot: "HOLDING".into(),
            mode: h.mode.clone(),
            pid: h.pid,
            label: h.label.clone(),
            agent: h.agent.clone(),
            cmd: h.cmd.clone(),
            time: h.elapsed,
            cpu: Some(h.cpu),
            rate: h.cpu_rate,
            eta: h.remaining,
            note,
            long,
            alarm,
            output: h.output.clone(),
            batch: h.batch.clone(),
        }
    }

    fn queued(machine: &str, q: &Queued) -> Item {
        let Verdict { note, long, alarm } = Verdict::of_queued(q);
        Item {
            machine: machine.to_string(),
            device: pinned(&q.device),
            holding: false,
            slot: format!("queued {}", q.position),
            mode: q.mode.clone(),
            pid: q.pid,
            label: q.label.clone(),
            agent: q.agent.clone(),
            cmd: q.cmd.clone(),
            time: q.waiting,
            cpu: None,
            rate: None,
            eta: q.eta,
            note,
            long,
            alarm,
            output: None, // it has no processes yet, so nothing to write with
            batch: q.batch.clone(),
        }
    }

    fn with_batch_step(mut self) -> Item {
        if let Some(b) = &self.batch {
            self.note = match b.left_text() {
                Some(l) => format!("{} · step {}/{}, {l} left", self.note, b.k, b.n),
                None => format!("{} · step {}/{}", self.note, b.k, b.n),
            };
        }
        self
    }
}

/// The feed writes `-` for a job that named no card.
fn pinned(device: &Option<String>) -> Option<String> {
    device.clone().filter(|d| d != "-" && !d.is_empty())
}

/// What the note column says about a job, what the pane says at length, and whether either
/// should catch the eye.
struct Verdict {
    note: String,
    long: String,
    alarm: bool,
}

impl Verdict {
    fn of_holder(h: &Holder) -> Verdict {
        if let Some(f) = h.idle_for {
            let never = h.idle_kind.as_deref() == Some("never");
            return Verdict {
                note: match never {
                    true => format!("no cpu at all in {}", dur(f)),
                    false => format!("{} cpu, none in {}", dur(h.cpu), dur(f)),
                },
                long: match never {
                    true => format!(
                        "It has burned no CPU at all in the {} since it acquired the lock: it is \
                         waiting on something. Stop it with K.",
                        dur(h.elapsed)
                    ),
                    false => format!(
                        "It has burned {} of CPU in total, but none of it in the last {}, so it has \
                         stopped doing anything. Stop it with K.",
                        dur(h.cpu),
                        dur(f)
                    ),
                },
                alarm: true,
            };
        }
        if h.overrun.unwrap_or(false) {
            return Verdict {
                note: format!("3x its usual {}", dur(h.est.unwrap_or(0))),
                long: format!(
                    "It has been running {}, more than three times the {} this same job usually \
                     takes over {} runs.",
                    dur(h.elapsed),
                    dur(h.est.unwrap_or(0)),
                    h.est_n.unwrap_or(0)
                ),
                alarm: true,
            };
        }
        let (Some(e), Some(n)) = (h.est, h.est_n) else {
            return Verdict {
                note: "no history for this one yet".into(),
                long: "Nothing recorded for this job or for its mode, so there is no honest \
                       estimate of how much longer it has."
                    .into(),
                alarm: false,
            };
        };
        let usual = if e == 0 {
            "under a second".to_string()
        } else {
            dur(e)
        };
        let (note, long) = match h.est_scope.as_deref().unwrap_or("this") {
            "this" => (
                format!("usually {usual} over {n} runs"),
                format!("This job usually takes {usual}, measured over {n} previous runs."),
            ),
            "agent" => (
                format!("this agent: {usual} over {n} runs"),
                format!(
                    "Nothing recorded under this label. This agent's other {} jobs take {usual} \
                     across {n} runs, which is the closest thing to an estimate there is.",
                    h.mode
                ),
            ),
            _ => (
                format!("no history; {} \u{2248} {usual}", h.mode),
                format!(
                    "Nothing recorded for this job or this agent. {} runs on this machine take \
                     {usual} as a rule, which is worth knowing but says nothing about this one.",
                    h.mode
                ),
            ),
        };
        Verdict {
            note,
            long,
            alarm: false,
        }
    }

    fn of_queued(q: &Queued) -> Verdict {
        let (note, long) = match q.eta {
            Some(e) => (
                format!("starts in ~{}", dur(e)),
                format!(
                    "Waiting for the lock. Nothing ahead of it is expected to take more than {}.",
                    dur(e)
                ),
            ),
            None => (
                "no telling when".into(),
                "Waiting for the lock. Something ahead of it is already past its usual \
                 duration, so there is no honest estimate of when this one starts."
                    .into(),
            ),
        };
        Verdict {
            note,
            long,
            alarm: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_in_a_batch_says_which_step_and_how_long_the_batch_has_left() {
        let s: Status = serde_json::from_str(
            r#"{"state":"shared","holders":[{"mode":"shared","pid":7,"label":"b","agent":"a","cmd":"c","elapsed":3,"cpu":1,"est":10,"est_n":3,"est_scope":"this","remaining":7,"batch":{"id":"20260917-1","step":"build","k":2,"n":5,"here":2,"elsewhere":1,"next":"bench ~4m00s","far":"home","left":620,"left_partial":true}}],"queue":[{"position":1,"mode":"bench","pid":8,"label":"q","agent":"x","cmd":"c","waiting":1,"eta":7}]}"#,
        )
        .unwrap();
        let v = Item::all("m", &s);
        assert_eq!(
            v[0].note,
            "usually 10s over 3 runs · step 2/5, over 10m20s left"
        );
        assert_eq!(
            v[1].note, "starts in ~7s",
            "a job outside a batch is unchanged"
        );
    }
}
