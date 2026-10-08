//! One row of the jobs table, and what the pane under it says about that job.

use dibs_format::{
    Alias, Label, Mode, Span,
    status::{BatchShown, Holder, IdleKind, Scope, Shown, Status, Waiter},
};

#[derive(Clone)]
pub struct Item {
    pub machine: String,
    /// Which card the job was pinned to, where one was named. Absent is the common case and
    /// is not a fault: a build does not want a card.
    pub device: Option<Alias>,
    pub holding: bool,
    pub slot: String,
    pub mode: Mode,
    pub pid: u32,
    pub label: Label,
    pub agent: String,
    pub cmd: String,
    pub time: u64,
    pub cpu: Option<u64>,
    pub rate: Option<u64>,
    pub eta: Option<u64>,
    pub note: String,
    /// The column is narrow and the pane is not, so each says as much as it has room for.
    pub long: String,
    pub alarm: bool,
    pub output: Option<String>,
    pub batch: Option<BatchShown>,
}

impl Item {
    /// Every job one machine's document lists, holders before the queue.
    pub fn all(machine: &str, s: &Status) -> Vec<Item> {
        s.holders
            .iter()
            .map(|h| Item::holding(machine, h))
            .chain(s.queue.iter().map(|q| Item::queued(machine, q)))
            .collect()
    }

    fn holding(machine: &str, h: &Holder) -> Item {
        let Verdict { note, long, alarm } = Verdict::of_holder(h);
        Item {
            machine: machine.to_string(),
            device: h.device.clone(),
            holding: true,
            slot: "HOLDING".into(),
            mode: h.mode,
            pid: h.pid,
            label: h.label.clone(),
            agent: h.agent.clone(),
            cmd: h.cmd.clone(),
            time: h.elapsed,
            cpu: Some(h.cpu),
            rate: h.cpu_rate,
            eta: h.estimate.as_ref().and_then(|e| e.remaining),
            note,
            long,
            alarm,
            output: h.output.clone(),
            batch: h.batch.clone(),
        }
    }

    fn queued(machine: &str, q: &Waiter) -> Item {
        let Verdict { note, long, alarm } = Verdict::of_queued(q);
        Item {
            machine: machine.to_string(),
            device: q.device.clone(),
            holding: false,
            slot: format!("queued {}", q.position),
            mode: q.mode,
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
}

fn runs(n: usize) -> String {
    match n {
        1 => "1 run".into(),
        n => format!("{n} runs"),
    }
}

/// How the table and the pane write a batch's progress.
pub trait Progress {
    /// Which step of how many, and how long the batch has left on this machine: `>` where that
    /// is a floor, `~` where it is an estimate.
    fn progress(&self) -> String;
    fn left_text(&self) -> Option<String>;
}

impl Progress for BatchShown {
    fn progress(&self) -> String {
        match &self.left {
            Some(l) if l.left_partial => format!("{}/{} >{}", self.k, self.n, Span(l.left)),
            Some(l) => format!("{}/{} ~{}", self.k, self.n, Span(l.left)),
            None => format!("{}/{}", self.k, self.n),
        }
    }

    fn left_text(&self) -> Option<String> {
        self.left.as_ref().map(|l| match l.left_partial {
            true => format!("over {}", Span(l.left)),
            false => format!("~{}", Span(l.left)),
        })
    }
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
        if let Some(idle) = &h.idle {
            let f = Span(idle.idle_for);
            let never = idle.idle_kind == IdleKind::Never;
            return Verdict {
                note: match never {
                    true => format!("no cpu at all in {f}"),
                    false => format!("{} cpu, none in {f}", Span(h.cpu)),
                },
                long: match never {
                    true => format!(
                        "It has burned no CPU at all in the {} since it acquired the lock: it is \
                         waiting on something. Stop it with K.",
                        Span(h.elapsed)
                    ),
                    false => format!(
                        "It has burned {} of CPU in total, but none of it in the last {f}, so it has \
                         stopped doing anything. Stop it with K.",
                        Span(h.cpu)
                    ),
                },
                alarm: true,
            };
        }
        let Some(shown) = &h.estimate else {
            return Verdict {
                note: "no history for this one yet".into(),
                long: "Nothing recorded for this job or for its mode, so there is no honest \
                       estimate of how much longer it has."
                    .into(),
                alarm: false,
            };
        };
        if let Some(overrun) = shown.overrun() {
            return Verdict {
                note: overrun.to_string(),
                long: format!(
                    "It has been running {}, over twice the 90th percentile of its {}, {}.",
                    Span(h.elapsed),
                    runs(shown.runs),
                    Span(overrun.high)
                ),
                alarm: true,
            };
        }
        Verdict::of_estimate(h.mode, shown)
    }

    fn of_estimate(mode: Mode, shown: &Shown) -> Verdict {
        let usual = match shown.median {
            0 => "under a second".to_string(),
            e => Span(e).to_string(),
        };
        let runs = runs(shown.runs);
        let (note, long) = match shown.scope {
            Scope::This if shown.other => (
                format!("other values: {usual} over {runs}"),
                format!(
                    "Nothing recorded with these values. Runs of this label with others take \
                     {usual} across {runs}, which may be a fraction of this one's work or a \
                     multiple of it."
                ),
            ),
            // One sample is a fact about one run, not a habit.
            Scope::This if shown.runs == 1 => (
                format!("ran once, in {usual}"),
                format!("This job has run once before, in {usual}."),
            ),
            Scope::This => (
                format!("usually {usual} over {runs}"),
                format!("This job usually takes {usual}, measured over {runs}."),
            ),
            Scope::Agent => (
                format!("this agent: {usual} over {runs}"),
                format!(
                    "Nothing recorded under this label. This agent's other {mode} jobs take {usual} \
                     across {runs}, which is the closest thing to an estimate there is."
                ),
            ),
            Scope::Mode => (
                format!("no history; {mode} \u{2248} {usual}"),
                format!(
                    "Nothing recorded for this job or this agent. {mode} runs on this machine take \
                     {usual} as a rule, which is worth knowing but says nothing about this one."
                ),
            ),
        };
        Verdict {
            note,
            long,
            alarm: false,
        }
    }

    fn of_queued(q: &Waiter) -> Verdict {
        let (note, long) = match q.eta {
            Some(e) => (
                format!("starts in ~{}", Span(e)),
                format!(
                    "Waiting for the lock. Nothing ahead of it is expected to take more than {}.",
                    Span(e)
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

    fn status(holders: &str, queue: &str) -> Status {
        serde_json::from_str(&format!(
            r#"{{"t":0,"state":"shared","cores":1,"load":0,"caches":[],"clones":[],"holders":[{holders}],"queue":[{queue}]}}"#
        ))
        .unwrap()
    }

    fn holder(label: &str, estimate: &str) -> String {
        format!(
            r#"{{"mode":"shared","pid":7,"label":"{label}","agent":"a","device":"-","cmd":"c","started":0,"elapsed":3,"cpu":1{estimate}}}"#
        )
    }

    #[test]
    fn a_job_in_a_batch_says_which_step_and_how_long_the_batch_has_left() {
        let batch = r#","batch":{"id":"20260917-1","step":"build","k":2,"n":5,"here":2,"elsewhere":1,"next":"bench ~4m00s","far":"home","left":620,"left_partial":true}"#;
        let s = status(
            &holder(
                "b",
                &format!(
                    r#","est":10,"est_lo":8,"est_hi":12,"est_n":3,"est_scope":"this","remaining":7{batch}"#
                ),
            ),
            r#"{"position":1,"mode":"bench","pid":8,"label":"q","agent":"x","device":"-","cmd":"c","arrived":0,"waiting":1,"eta":7}"#,
        );
        let v = Item::all("m", &s);
        assert_eq!(v[0].note, "usually 10s over 3 runs");
        assert_eq!(
            v[0].batch.as_ref().map(Progress::progress).as_deref(),
            Some("2/5 >10m20s")
        );
        assert!(v[1].batch.is_none(), "a job outside a batch has no step");
    }

    #[test]
    fn one_run_is_not_called_usual() {
        let once = r#","est":116,"est_lo":116,"est_hi":116,"est_n":1"#;
        let s = status(
            &format!(
                "{},{}",
                holder("b", &format!(r#"{once},"est_scope":"this""#)),
                holder("c", &format!(r#"{once},"est_scope":"agent""#))
            ),
            "",
        );
        let v = Item::all("m", &s);
        assert_eq!(v[0].note, "ran once, in 1m56s");
        assert_eq!(v[1].note, "this agent: 1m56s over 1 run");
    }
}
