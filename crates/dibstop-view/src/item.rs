//! One row of the jobs table, and what the pane under it says about that job.

use dibs_format::{
    Alias, JobId, Label, Mode, Span,
    status::{
        BatchShown, Holder, HolderState, IdleKind, Idleness, Overrun, PreemptionState, Scope,
        ShownEstimate, Silence, Status, Waiter,
    },
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
    /// Whose log on the machine `dibs out` reads, for a job holding the lock.
    pub job: Option<JobId>,
    pub label: Label,
    pub agent: String,
    /// The person the agent runs as, where its call said.
    pub user: Option<String>,
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
    pub priority: Option<Priority>,
}

/// A job asked to go ahead of the others, or to give way to all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Urgent,
    Low,
}

impl Priority {
    fn of(urgent: bool, low: bool) -> Option<Priority> {
        match (urgent, low) {
            (true, _) => Some(Priority::Urgent),
            (false, true) => Some(Priority::Low),
            (false, false) => None,
        }
    }
}

impl Item {
    /// The agent, and the person it runs as, as `dibs status` writes them.
    pub fn who(&self) -> String {
        match &self.user {
            Some(user) => format!("{} ({user})", self.agent),
            None => self.agent.clone(),
        }
    }

    /// Whether there is output to read: a file the job writes to, or else the log of its job,
    /// which a transfer does not keep.
    pub fn readable(&self) -> bool {
        self.holding && (self.output.is_some() || (self.job.is_some() && self.mode != Mode::Rsh))
    }

    /// Every job one machine's document lists, holders before the queue.
    pub fn all(machine: &str, s: &Status) -> Vec<Item> {
        s.holders
            .iter()
            .map(|h| Item::holding(machine, h))
            .chain(s.queue.iter().map(|q| Item::queued(machine, q)))
            .collect()
    }

    fn holding(machine: &str, h: &Holder) -> Item {
        let Verdict {
            note,
            mut long,
            alarm,
        } = Verdict::of_holder(h);
        let priority = Priority::of(h.urgent.is_some(), h.low_priority);
        let said = match (&h.urgent, priority, h.call.mode) {
            (Some(why), _, Mode::Shared) => {
                format!("Urgent: {why}. It went around the queued benchmarks. ")
            }
            (Some(why), _, _) => format!("Urgent: {why}. The shared jobs are parked for it. "),
            (None, Some(Priority::Low), Mode::Rsh) => "Low priority: it waited for an idle \
                machine, and a copy is never parked, so it finishes once begun. "
                .into(),
            (None, Some(Priority::Low), _) => "Low priority: parked as soon as a job that is not \
                low priority comes, to run again from the start once the machine is idle. "
                .into(),
            _ => String::new(),
        };
        long.insert_str(0, &said);
        Item {
            machine: machine.to_string(),
            device: h.call.device.clone(),
            holding: true,
            slot: match (&h.ahead, priority) {
                (Some(PreemptionState::Paused(_)), _) => "FROZEN".into(),
                (_, Some(Priority::Urgent)) => "URGENT".into(),
                (_, Some(Priority::Low)) => "LOW PRIO".into(),
                _ => "HOLDING".into(),
            },
            mode: h.call.mode,
            pid: h.call.pid,
            job: h.job.clone(),
            label: h.call.label.clone(),
            agent: h.call.agent.clone(),
            user: h.call.user.clone(),
            cmd: h.call.cmd.clone(),
            time: h.elapsed_secs,
            cpu: Some(h.cpu_secs),
            rate: h.cpu_rate,
            eta: h.estimate.as_ref().and_then(|e| e.remaining),
            note,
            long,
            alarm,
            output: h.output.clone(),
            batch: h.batch.clone(),
            priority,
        }
    }

    fn queued(machine: &str, q: &Waiter) -> Item {
        let Verdict { note, long, alarm } = Verdict::of_queued(q);
        Item {
            machine: machine.to_string(),
            device: q.call.device.clone(),
            holding: false,
            slot: match Priority::of(q.urgent.is_some(), q.low_priority) {
                Some(Priority::Urgent) => format!("urgent {}", q.position),
                Some(Priority::Low) => format!("low prio {}", q.position),
                None => format!("queued {}", q.position),
            },
            mode: q.call.mode,
            pid: q.call.pid,
            job: None,
            label: q.call.label.clone(),
            agent: q.call.agent.clone(),
            user: q.call.user.clone(),
            cmd: q.call.cmd.clone(),
            time: q.waiting_secs,
            cpu: None,
            rate: None,
            eta: q.eta,
            note,
            long,
            alarm,
            output: None, // it has no processes yet, so nothing to write with
            batch: q.batch.clone(),
            priority: Priority::of(q.urgent.is_some(), q.low_priority),
        }
    }
}

/// A low-priority waiter's note and pane, around what it `waits` for.
fn low_priority(waits: String) -> (String, String) {
    (
        format!("low priority: {waits}"),
        format!(
            "Low priority: it starts only once nothing but other low-priority work or a \
             transfer runs or waits on the machine. It {waits}."
        ),
    )
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
    /// is a floor, `~` where it is an estimate. A recipe step's own job count is the pane's,
    /// since a second bare count in one cell reads as part of the first.
    fn progress(&self) -> String;
    fn left_text(&self) -> Option<String>;
}

impl Progress for BatchShown {
    fn progress(&self) -> String {
        let step = format!("step {}/{}", self.k, self.step_count);
        match &self.left {
            Some(l) if l.left_partial => format!("{step}, >{} left", Span::from_secs(l.left_secs)),
            Some(l) => format!("{step}, ~{} left", Span::from_secs(l.left_secs)),
            None => step,
        }
    }

    fn left_text(&self) -> Option<String> {
        self.left.as_ref().map(|l| match l.left_partial {
            true => format!("over {}", Span::from_secs(l.left_secs)),
            false => format!("~{}", Span::from_secs(l.left_secs)),
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
    /// In this view's words, what `HolderState` says comes first.
    fn of_holder(h: &Holder) -> Verdict {
        match HolderState::of(h) {
            HolderState::UrgentAlone(why) => Verdict::urgent_alone(why),
            HolderState::Parked(urgent) => Verdict::parked(urgent),
            HolderState::Frozen(bench) => Verdict::frozen(bench),
            HolderState::Preparing(doing) => Verdict::preparing(doing),
            HolderState::Idle(idle) => Verdict::idle(h, idle),
            HolderState::Silent(silent) => Verdict::silent(silent),
            HolderState::Overrun(overrun, shown) => Verdict::overrun(h, overrun, shown),
            HolderState::Estimated(shown) => Verdict::of_estimate(h.call.mode, shown),
            HolderState::Unestimated => Verdict::unestimated(),
        }
    }

    fn urgent_alone(why: &str) -> Verdict {
        Verdict {
            note: format!("urgent: {why}"),
            long: "Nothing else runs here until it ends.".into(),
            alarm: false,
        }
    }

    fn parked(urgent: &JobId) -> Verdict {
        Verdict {
            note: format!("parked for urgent job {urgent}"),
            long: format!(
                "It is parked while the urgent job {urgent} runs, and runs again from the \
                 start after it. It is waiting its turn, not stuck."
            ),
            alarm: false,
        }
    }

    fn frozen(bench: &JobId) -> Verdict {
        Verdict {
            note: format!("frozen for benchmark {bench}"),
            long: format!(
                "It only compiles, and the benchmark {bench} was queued before it, so it is \
                 frozen while that benchmark runs and picks up where it stopped afterwards. It \
                 is waiting its turn, not stuck."
            ),
            alarm: false,
        }
    }

    fn preparing(doing: &str) -> Verdict {
        Verdict {
            note: format!("preparing: {doing}"),
            long: format!(
                "The machine is laying out its tree before the command runs, and said last: \
                 {doing}. A copy or a cleanup on a slow disk can take minutes, none of it the \
                 command's."
            ),
            alarm: false,
        }
    }

    fn idle(h: &Holder, idle: &Idleness) -> Verdict {
        let f = Span::from_secs(idle.idle_for_secs);
        let never = idle.idle_kind == IdleKind::Never;
        Verdict {
            note: match never {
                true => format!("no cpu at all in {f}"),
                false => format!("{} cpu, none in {f}", Span::from_secs(h.cpu_secs)),
            },
            long: match never {
                true => format!(
                    "It has burned no CPU at all in the {} since it acquired the lock: it is \
                     waiting on something. Stop it with K.",
                    Span::from_secs(h.elapsed_secs)
                ),
                false => format!(
                    "It has burned {} of CPU in total, but none of it in the last {f}, so it has \
                     stopped doing anything. Stop it with K.",
                    Span::from_secs(h.cpu_secs)
                ),
            },
            alarm: true,
        }
    }

    fn silent(silent: &Silence) -> Verdict {
        let last = match silent.silent_line.as_str() {
            "" => String::new(),
            line => format!(" Its last line: {line}"),
        };
        Verdict {
            note: silent.to_text(),
            long: format!(
                "Its output has not changed in {}, longer than its own history accounts for, \
                 so it may be hung rather than slow.{last} Stop it with K.",
                Span::from_secs(silent.silent_for_secs)
            ),
            alarm: true,
        }
    }

    fn overrun(h: &Holder, overrun: Overrun, shown: &ShownEstimate) -> Verdict {
        Verdict {
            note: overrun.to_text(),
            long: format!(
                "It has been running {}, over twice the 90th percentile of its {}, {}.",
                Span::from_secs(h.elapsed_secs),
                runs(shown.run_count),
                Span::from_secs(overrun.high_secs)
            ),
            alarm: true,
        }
    }

    fn unestimated() -> Verdict {
        Verdict {
            note: "no history for this one yet".into(),
            long: "Nothing recorded for this job or for its mode, so there is no honest \
                   estimate of how much longer it has."
                .into(),
            alarm: false,
        }
    }

    fn of_estimate(mode: Mode, shown: &ShownEstimate) -> Verdict {
        let usual = match shown.median_secs {
            0 => "under a second".to_string(),
            e => Span::from_secs(e).to_string(),
        };
        let runs = runs(shown.run_count);
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
            Scope::This if shown.run_count == 1 => (
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
        let (note, long) = match (q.low_priority, &q.idle_wait, q.eta) {
            (true, Some(wait), _) => low_priority(wait.to_text()),
            // From a runner older than `idle_wait`.
            (true, None, _) => low_priority("waits for an idle machine".into()),
            (false, _, Some(e)) => (
                format!("starts in ~{}", Span::from_secs(e)),
                format!(
                    "Waiting for the lock. Nothing ahead of it is expected to take more than {}.",
                    Span::from_secs(e)
                ),
            ),
            (false, _, None) => (
                "no telling when".into(),
                "Waiting for the lock. Something ahead of it is already past its usual \
                 duration, so there is no honest estimate of when this one starts."
                    .into(),
            ),
        };
        let Some(why) = &q.urgent else {
            return Verdict {
                note,
                long,
                alarm: false,
            };
        };
        let does = match q.call.mode {
            Mode::Shared => "It goes around the queued benchmarks.",
            _ => {
                "Once it holds the lock, the shared jobs are parked for it and run again from \
                  the start after it."
            }
        };
        Verdict {
            note: format!("urgent: {why}, {note}"),
            long: format!("Urgent: {why}. {does} {long}"),
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
        let batch = r#","batch":{"id":"20260917-1","step":"build","k":2,"n":5,"here":2,"next":"bench ~4m00s","left":620,"left_partial":true}"#;
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
            Some("step 2/5, >10m20s left")
        );
        assert!(v[1].batch.is_none(), "a job outside a batch has no step");
        let preparing = status(
            &holder(
                "b",
                r#","est":1,"est_lo":1,"est_hi":1,"est_n":9,"est_scope":"this","prepare":"""#,
            ),
            "",
        );
        let v = Item::all("m", &preparing);
        assert_eq!(
            (v[0].note.as_str(), v[0].alarm),
            ("preparing: laying out its tree", false),
            "a prepare is not timed against the command's history"
        );
    }

    #[test]
    fn a_job_writing_to_no_file_of_its_own_is_read_from_its_log() {
        let s = status(&holder("b", r#","job":"20261123211956-7""#), "");
        let v = Item::all("m", &s);
        assert!(v[0].readable());
        assert_eq!(
            crate::action::Action::output(&v[0]).args,
            ["--out", "20261123211956-7"]
        );
        let odd = crate::action::Action::new(
            "m".into(),
            crate::action::Call::JobLog {
                pid: 7,
                job: "--help".into(),
            },
            "b",
        );
        assert_eq!(
            odd.args,
            ["--out", "7"],
            "a job id that is not one goes as the pid"
        );
    }

    #[test]
    fn a_job_names_the_person_its_agent_runs_as() {
        let s = status(
            &holder("b", r#","user":"nathaniel""#),
            r#"{"position":1,"mode":"bench","pid":8,"label":"q","agent":"x","device":"-","cmd":"c","arrived":0,"waiting":1}"#,
        );
        let v = Item::all("m", &s);
        assert_eq!(v[0].who(), format!("{} (nathaniel)", v[0].agent));
        assert_eq!(v[1].who(), "x");
    }

    #[test]
    fn a_frozen_job_is_waiting_its_turn_not_idle() {
        let idle = r#","idle_for":600,"idle_kind":"never""#;
        let frozen = status(
            &holder(
                "b",
                &format!(r#"{idle},"ahead":{{"paused":"20261103120000-9"}}"#),
            ),
            "",
        );
        let v = Item::all("m", &frozen);
        assert_eq!(v[0].slot, "FROZEN");
        assert_eq!(
            (v[0].note.as_str(), v[0].alarm),
            ("frozen for benchmark 20261103120000-9", false)
        );
        let running = status(&holder("b", r#","ahead":"running""#), "");
        assert_eq!(Item::all("m", &running)[0].slot, "HOLDING");
    }

    const IDLE: &str = r#","idle_for":600,"idle_kind":"never""#;

    #[test]
    fn an_urgent_benchmark_is_said_urgent_before_idle() {
        let urgent = holder("b", &format!(r#"{IDLE},"urgent":"demo""#))
            .replace(r#""mode":"shared""#, r#""mode":"bench""#);
        let v = Item::all("m", &status(&urgent, ""));
        assert_eq!((v[0].note.as_str(), v[0].alarm), ("urgent: demo", false));
    }

    #[test]
    fn a_parked_job_is_waiting_its_turn_not_idle() {
        let parked = holder("b", &format!(r#"{IDLE},"parked":"20261103120000-9""#));
        let v = Item::all("m", &status(&parked, ""));
        assert_eq!(
            (v[0].note.as_str(), v[0].alarm),
            ("parked for urgent job 20261103120000-9", false)
        );
    }

    #[test]
    fn a_preparing_job_is_said_preparing_before_idle() {
        let preparing = holder("b", &format!(r#"{IDLE},"prepare":"copying""#));
        let v = Item::all("m", &status(&preparing, ""));
        assert_eq!(
            (v[0].note.as_str(), v[0].alarm),
            ("preparing: copying", false)
        );
    }

    #[test]
    fn an_idle_job_is_said_idle_and_alarms() {
        let v = Item::all("m", &status(&holder("b", IDLE), ""));
        assert_eq!(
            (v[0].note.as_str(), v[0].alarm),
            ("no cpu at all in 10m00s", true)
        );
    }

    #[test]
    fn a_job_says_whether_it_is_urgent_or_low_priority() {
        let s = status(
            &format!(
                "{},{}",
                holder("u", r#","urgent":"demo at 3""#),
                holder("l", r#","low_priority":true"#)
            ),
            r#"{"position":1,"mode":"bench","pid":8,"label":"q","agent":"x","device":"-","cmd":"c","arrived":0,"waiting":1,"urgent":"release"},
               {"position":2,"mode":"shared","pid":9,"label":"r","agent":"x","device":"-","cmd":"c","arrived":0,"waiting":1,"low_priority":true}"#,
        );
        let v = Item::all("m", &s);
        let seen: Vec<_> = v.iter().map(|it| (it.slot.as_str(), it.priority)).collect();
        assert_eq!(
            seen,
            [
                ("URGENT", Some(Priority::Urgent)),
                ("LOW PRIO", Some(Priority::Low)),
                ("urgent 1", Some(Priority::Urgent)),
                ("low prio 2", Some(Priority::Low)),
            ]
        );
        assert_eq!(v[2].note, "urgent: release, no telling when");
        assert_eq!(v[3].note, "low priority: waits for an idle machine");
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
