use crate::{Alias, BatchId, JobId, Label, Mode, Span, json::json_line};
use serde::{Deserialize, Serialize};

/// One machine's lock as `dibs status --json` writes it: who holds it, who waits and when each
/// should start, and what a dispatcher ranks the machine by. dibstop reads it. It is also a
/// pinned interface for agents, so a field no dibs program reads, such as a holder's `subagent`
/// or `started`, stays in it. A runner writes `state` as `bench` or `shared` when `holders` is
/// not empty, and as neither when it is.
///
/// # Guarantees
///
/// - `to_line` writes it as one line of JSON, which reads back equal.
/// - A reader passes over a field it does not know, reads `caches` and `clones` as empty when
///   they are absent, and reads a lock state it does not know as [`LockState::Other`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// When it was read, in seconds since the epoch.
    #[serde(rename = "t")]
    pub read_secs: u64,
    pub state: LockState,
    #[serde(rename = "cores")]
    pub core_count: u64,
    /// The one-minute load average.
    #[serde(rename = "load")]
    pub load_hundredths: u64,
    /// The repos it has a build cache for, in name order. Only written: placement reads them
    /// through its own reading, asking whether they hold a repo.
    #[serde(default)]
    pub caches: Vec<String>,
    /// The repos it has a clone of, which a worktree can be prepared from, in name order. Only
    /// written: placement reads them through its own reading, asking whether they hold a repo.
    #[serde(default)]
    pub clones: Vec<String>,
    /// In the order the lock records list them, and read in it.
    pub holders: Vec<Holder>,
    /// In arrival order, and read in it.
    pub queue: Vec<Waiter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<Closure>,
    /// Only for a caller that asked for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<super::Metrics>,
    /// The labels measured here and the card each was measured on, so placement keeps a
    /// series where it began whoever's call it is. Only written, in the order the runner keeps
    /// them: placement reads them through its own reading.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub series: Vec<SeriesHere>,
}

/// One label's series on a machine, as its runner keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesHere {
    pub label: String,
    /// The cards, comma-separated, or `none` for a measurement that named none.
    pub card: String,
    /// When it was last measured, in seconds since the epoch.
    #[serde(rename = "at")]
    pub at_secs: u64,
}

impl Status {
    pub fn to_line(&self) -> String {
        json_line(self)
    }
}

/// A machine its owners have taken back, as `closed` in the lock directory says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closure {
    /// The account that closed it.
    pub by: String,
    /// In seconds since the epoch.
    #[serde(rename = "since")]
    pub since_secs: u64,
    #[serde(flatten)]
    pub close: crate::wire::Close,
}

/// Who holds the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockState {
    Idle,
    Shared,
    Bench,
    /// Taken with no holder record yet, as by a client between taking it and recording it.
    Busy,
    /// Taken by a process no record accounts for, which outlived its session.
    Orphan,
    /// A state a newer runner wrote that this dibs does not know; no runner writes `other`.
    #[serde(other)]
    Other,
}

/// The call a job holding or waiting for the lock was started by, as its lock record and the
/// files beside it say.
///
/// # Guarantees
///
/// - `device` is written as `-` when it names no card, as the lock records write it; `-` and an
///   empty string read back as none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCall {
    pub mode: Mode,
    pub pid: u32,
    pub label: Label,
    pub agent: String,
    /// The person the agent runs as on its own computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// The Claude subagent within the agent's session that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    #[serde(default, with = "dashed")]
    pub device: Option<Alias>,
    pub cmd: String,
    /// The whole command, where `cmd` cut it short.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// A job holding the lock. A runner writes `idle` only once its tree has used no CPU for longer
/// than its `idle_after`, and only where [`HolderState::of`] then says [`HolderState::Idle`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    #[serde(flatten)]
    pub call: JobCall,
    /// None for a job a bash machine half started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<JobId>,
    /// When it took the lock, in seconds since the epoch.
    #[serde(rename = "started")]
    pub started_secs: u64,
    #[serde(rename = "elapsed")]
    pub elapsed_secs: u64,
    /// The `elapsed_secs` at which its cap starts stopping it, where it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killed_at: Option<u64>,
    /// What its whole tree has used, reaped children included.
    #[serde(rename = "cpu")]
    pub cpu_secs: u64,
    #[serde(flatten)]
    pub estimate: Option<ShownEstimate>,
    /// The first file its tree writes its output to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Cores' worth of CPU, in hundredths, since the last look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_rate: Option<u64>,
    #[serde(flatten)]
    pub idle: Option<Idleness>,
    #[serde(flatten)]
    pub silent: Option<Silence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchShown>,
    /// What the machine last said while it lays out the job's tree, which comes before the
    /// command and none of whose time is the command's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare: Option<String>,
    /// Preemptible: a benchmark queued before it freezes it while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead: Option<PreemptionState>,
    /// Why it is urgent: a benchmark has the machine to itself, the shared jobs parked; a shared
    /// job only went around the queued benchmarks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgent: Option<String>,
    /// The urgent job it runs nothing for until that ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<JobId>,
    /// `--low-priority`: parked, to run again from the start, as soon as another job comes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub low_priority: bool,
}

/// Where a preemptible job is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreemptionState {
    Running,
    /// Frozen while that benchmark runs.
    Paused(JobId),
}

impl Holder {
    /// What it is doing, while its tree is laid out.
    pub fn preparing(&self) -> Option<&str> {
        self.prepare.as_deref().map(|said| match said {
            "" => "laying out its tree",
            said => said,
        })
    }

    /// Why it is urgent, where that gives it the machine to itself and parks the shared jobs.
    pub fn urgent_alone(&self) -> Option<&str> {
        self.urgent
            .as_deref()
            .filter(|_| self.call.mode != Mode::Shared)
    }
}

/// What a holder's line says first, which every view of it says in its own words: the first of
/// these that holds, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderState<'a> {
    /// Urgent with the machine to itself, for this reason.
    UrgentAlone(&'a str),
    /// Parked until this urgent job ends.
    Parked(&'a JobId),
    /// Frozen while this benchmark, queued before it, runs.
    Frozen(&'a JobId),
    /// Its tree is being laid out, and the machine said this last.
    Preparing(&'a str),
    Idle(&'a Idleness),
    /// Its output has stopped for longer than its history accounts for.
    Silent(&'a Silence),
    /// Running more than twice its ninetieth percentile.
    Overrun(Overrun, &'a ShownEstimate),
    /// Running as its history says it should.
    Estimated(&'a ShownEstimate),
    /// Running, with no history to say how long it takes.
    Unestimated,
}

impl<'a> HolderState<'a> {
    pub fn of(holder: &'a Holder) -> HolderState<'a> {
        let overrun = holder.estimate.as_ref().and_then(ShownEstimate::overrun);
        match (
            holder.urgent_alone(),
            &holder.parked,
            &holder.ahead,
            holder.preparing(),
            &holder.idle,
            &holder.silent,
            overrun,
            &holder.estimate,
        ) {
            (Some(why), ..) => HolderState::UrgentAlone(why),
            (None, Some(urgent), ..) => HolderState::Parked(urgent),
            (None, None, Some(PreemptionState::Paused(bench)), ..) => HolderState::Frozen(bench),
            (None, None, _, Some(doing), ..) => HolderState::Preparing(doing),
            (None, None, _, None, Some(idle), ..) => HolderState::Idle(idle),
            (None, None, _, None, None, Some(silent), ..) => HolderState::Silent(silent),
            (None, None, _, None, None, None, Some(overrun), Some(shown)) => {
                HolderState::Overrun(overrun, shown)
            }
            (None, None, _, None, None, None, _, Some(shown)) => HolderState::Estimated(shown),
            (None, None, _, None, None, None, _, None) => HolderState::Unestimated,
        }
    }
}

/// A job waiting for the lock. A runner writes `idle_wait` exactly when it writes
/// `low_priority`; one older than `idle_wait` wrote `low_priority` alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiter {
    /// Counting from 1, in arrival order.
    pub position: usize,
    #[serde(flatten)]
    pub call: JobCall,
    /// When it arrived, in seconds since the epoch.
    #[serde(rename = "arrived")]
    pub arrived_secs: u64,
    #[serde(rename = "waiting")]
    pub waiting_secs: u64,
    /// Seconds until it should start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta: Option<u64>,
    /// `--low-priority`: it starts only once nothing else runs or waits, so it has no place in line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub low_priority: bool,
    /// Why it is urgent: a benchmark parks the shared jobs once it holds the lock; a shared job
    /// goes around the queued benchmarks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_wait: Option<IdleWait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchShown>,
}

/// Where a `--low-priority` job's wait for an idle machine stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleWait {
    /// The job it waits behind, as `<label> from <agent>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind: Option<String>,
    /// How long nothing else has run or waited here, of the `settle_secs` it needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_for: Option<u64>,
    #[serde(rename = "settle")]
    pub settle_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<LowPriorityStop>,
}

/// A `--low-priority` job parked after it ran, to run again from the start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LowPriorityStop {
    #[serde(rename = "ran")]
    pub ran_secs: u64,
    #[serde(rename = "ago")]
    pub ago_secs: u64,
    /// The job it was parked for, as `<label> from <agent>`.
    pub by: String,
}

impl IdleWait {
    /// What it waits for, and what it ran before it was parked: the job it waits behind, or
    /// else how long the machine has been quiet.
    pub fn to_text(&self) -> String {
        format!(
            "waits for an idle machine{}{}",
            self.to_waited_on(),
            self.stopped
                .as_ref()
                .map(LowPriorityStop::to_clause)
                .unwrap_or_default()
        )
    }
}

impl IdleWait {
    fn to_waited_on(&self) -> String {
        match (&self.behind, self.quiet_for) {
            (Some(behind), _) => format!(", behind {behind}"),
            (None, Some(quiet)) => format!(
                ", quiet {} of {}",
                Span::from_secs(quiet),
                Span::from_secs(self.settle_secs)
            ),
            (None, None) => String::new(),
        }
    }
}

impl LowPriorityStop {
    fn to_clause(&self) -> String {
        format!(
            "; ran {} until {} came {} ago, and runs again from the start",
            Span::from_secs(self.ran_secs),
            self.by,
            Span::from_secs(self.ago_secs)
        )
    }
}

/// What a holder's history says it takes, and where that leaves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownEstimate {
    #[serde(rename = "est")]
    pub median_secs: u64,
    /// The tenth percentile.
    #[serde(rename = "est_lo")]
    pub low_secs: u64,
    /// The ninetieth percentile.
    #[serde(rename = "est_hi")]
    pub high_secs: u64,
    #[serde(rename = "est_n")]
    pub run_count: usize,
    #[serde(rename = "est_scope")]
    pub scope: Scope,
    /// The runs spread too far for the median to predict anything.
    #[serde(rename = "est_wide", default, skip_serializing_if = "is_false")]
    pub wide: bool,
    /// Drawn from the label's other procedures, since this one has not run.
    #[serde(rename = "est_other_values", default, skip_serializing_if = "is_false")]
    pub other: bool,
    /// Seconds left; None once it has run longer than its history says it ever takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_kind: Option<RemainingBasis>,
    /// Running more than twice its ninetieth percentile.
    #[serde(default, skip_serializing_if = "is_false")]
    pub overrun: bool,
}

/// A holder running more than twice its ninetieth percentile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overrun {
    pub high_secs: u64,
}

impl ShownEstimate {
    pub fn overrun(&self) -> Option<Overrun> {
        self.overrun.then_some(Overrun {
            high_secs: self.high_secs,
        })
    }
}

impl Overrun {
    /// In the words every view of it uses.
    pub fn to_text(&self) -> String {
        format!("over 2x its p90, {}", Span::from_secs(self.high_secs))
    }
}

/// Which key an estimate was drawn from, sharpest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// This label, and this procedure where the job named one.
    This,
    /// What the agent's jobs in this mode usually take.
    Agent,
    /// What any job in this mode takes.
    Mode,
}

/// What the time left is measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemainingBasis {
    /// The median, not yet reached.
    Typical,
    /// The ninetieth percentile, once past the median.
    Bound,
}

/// A holder whose tree has used no CPU for a while.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Idleness {
    #[serde(rename = "idle_for")]
    pub idle_for_secs: u64,
    pub idle_kind: IdleKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdleKind {
    /// No CPU at all since it started.
    Never,
    /// None since the last look that saw it working.
    Stalled,
}

/// A holder whose output has not changed for longer than its own history accounts for, which a
/// hang that keeps spinning looks like as much as one that sleeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Silence {
    #[serde(rename = "silent_for")]
    pub silent_for_secs: u64,
    /// The last line it wrote, empty when it wrote none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub silent_line: String,
}

impl Silence {
    /// In the words every view of it uses.
    pub fn to_text(&self) -> String {
        format!("no output in {}", Span::from_secs(self.silent_for_secs))
    }
}

/// The batch a job is a step of, and what the batch still has to do on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchShown {
    pub id: BatchId,
    pub step: String,
    /// Which step this is, counting from 1.
    pub k: usize,
    #[serde(rename = "n")]
    pub step_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<RecipePlace>,
    /// Steps still to come here.
    #[serde(rename = "here")]
    pub here_count: usize,
    /// Those steps by name, each with its estimate.
    pub next: String,
    #[serde(flatten)]
    pub left: Option<BatchTimeLeft>,
}

/// A job's place among its recipe's jobs, when the recipe is one step of a batch: the step's own
/// count stays put for as long as all of them take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipePlace {
    /// Which of them it is, counting from 1.
    pub job: usize,
    #[serde(rename = "jobs")]
    pub job_count: usize,
}

/// How long the batch has left on this machine, queue included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchTimeLeft {
    #[serde(rename = "left")]
    pub left_secs: u64,
    /// Some of what is ahead has no history, so this is a floor.
    pub left_partial: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A card as the records write it: `-` for none.
mod dashed {
    use crate::Alias;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<Alias>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(value.as_ref().map_or("-", Alias::as_str))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Alias>, D::Error> {
        let text = String::deserialize(d)?;
        Ok(match text.as_str() {
            "-" | "" => None,
            _ => Some(Alias::new(text)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A holder's line as a runner before `caches` and `clones` wrote it.
    const OLDER: &str = r#"{"t":5,"state":"shared","cores":2,"load":30,"holders":[{"mode":"shared","pid":7,"label":"b","agent":"a","device":"-","cmd":"c","started":1,"elapsed":4,"cpu":0}],"queue":[]}"#;

    fn call(device: Option<&str>) -> JobCall {
        JobCall {
            mode: Mode::Shared,
            pid: 7,
            label: Label::new("b"),
            agent: "a".into(),
            user: None,
            subagent: None,
            device: device.map(Alias::new),
            cmd: "c".into(),
            command: None,
        }
    }

    fn holder(mode: Mode) -> Holder {
        Holder {
            call: JobCall { mode, ..call(None) },
            job: None,
            started_secs: 1,
            elapsed_secs: 4,
            killed_at: None,
            cpu_secs: 0,
            estimate: None,
            output: None,
            cpu_rate: None,
            idle: None,
            silent: None,
            batch: None,
            prepare: None,
            ahead: None,
            urgent: None,
            parked: None,
            low_priority: false,
        }
    }

    fn estimate(overrun: bool) -> ShownEstimate {
        ShownEstimate {
            median_secs: 10,
            low_secs: 8,
            high_secs: 60,
            run_count: 3,
            scope: Scope::This,
            wide: false,
            other: false,
            remaining: None,
            remaining_kind: None,
            overrun,
        }
    }

    fn wait() -> IdleWait {
        IdleWait {
            behind: None,
            quiet_for: None,
            settle_secs: 120,
            stopped: None,
        }
    }

    #[test]
    fn an_older_runners_status_reads_with_no_caches_or_clones() {
        let status: Status = serde_json::from_str(OLDER).unwrap();
        assert_eq!(
            (
                status.caches.len(),
                status.clones.len(),
                status.holders.len()
            ),
            (0, 0, 1)
        );
    }

    #[test]
    fn a_state_this_dibs_does_not_know_reads_as_other() {
        let newer = OLDER.replace(r#""state":"shared""#, r#""state":"draining""#);
        let status: Status = serde_json::from_str(&newer).unwrap();
        assert_eq!(status.state, LockState::Other);
    }

    #[test]
    fn a_status_reads_back_what_its_line_wrote() {
        let status: Status = serde_json::from_str(OLDER).unwrap();
        let line = status.to_line();
        assert_eq!(line.matches('\n').count(), 1);
        assert_eq!(serde_json::from_str::<Status>(&line).unwrap(), status);
    }

    #[test]
    fn a_call_naming_no_card_writes_a_dash() {
        let written = serde_json::to_string(&call(None)).unwrap();
        assert!(written.contains(r#""device":"-""#), "{written}");
    }

    #[test]
    fn a_dash_and_an_empty_card_read_as_none() {
        let dashed: JobCall =
            serde_json::from_str(&serde_json::to_string(&call(None)).unwrap()).unwrap();
        let empty: JobCall = serde_json::from_str(
            r#"{"mode":"shared","pid":7,"label":"b","agent":"a","device":"","cmd":"c"}"#,
        )
        .unwrap();
        assert_eq!((dashed.device, empty.device), (None, None));
    }

    #[test]
    fn a_named_card_reads_back_as_its_alias() {
        let named = call(Some("gpu:a"));
        let read: JobCall = serde_json::from_str(&serde_json::to_string(&named).unwrap()).unwrap();
        assert_eq!(read.device, Some(Alias::new("gpu:a")));
    }

    #[test]
    fn an_empty_prepare_says_the_tree_is_being_laid_out() {
        let mut h = holder(Mode::Shared);
        h.prepare = Some(String::new());
        assert_eq!(h.preparing(), Some("laying out its tree"));
    }

    #[test]
    fn a_prepare_says_what_the_machine_said() {
        let mut h = holder(Mode::Shared);
        h.prepare = Some("copying a target".into());
        assert_eq!(h.preparing(), Some("copying a target"));
    }

    #[test]
    fn a_holder_with_no_prepare_is_not_preparing() {
        assert_eq!(holder(Mode::Shared).preparing(), None);
    }

    #[test]
    fn an_urgent_benchmark_has_the_machine_to_itself() {
        let mut h = holder(Mode::Bench);
        h.urgent = Some("demo".into());
        assert_eq!(h.urgent_alone(), Some("demo"));
    }

    #[test]
    fn an_urgent_shared_job_only_went_around_the_queue() {
        let mut h = holder(Mode::Shared);
        h.urgent = Some("demo".into());
        assert_eq!(h.urgent_alone(), None);
    }

    fn bench() -> JobId {
        JobId::new("20261001120000-9")
    }

    fn idleness() -> Idleness {
        Idleness {
            idle_for_secs: 600,
            idle_kind: IdleKind::Never,
        }
    }

    fn silence() -> Silence {
        Silence {
            silent_for_secs: 3000,
            silent_line: String::new(),
        }
    }

    #[test]
    fn an_urgent_benchmark_is_urgent_over_being_parked() {
        let mut h = holder(Mode::Bench);
        h.urgent = Some("demo".into());
        h.parked = Some(bench());
        assert_eq!(HolderState::of(&h), HolderState::UrgentAlone("demo"));
    }

    #[test]
    fn a_parked_holder_is_parked_over_being_frozen() {
        let mut h = holder(Mode::Shared);
        h.parked = Some(bench());
        h.ahead = Some(PreemptionState::Paused(JobId::new("20261001120000-8")));
        assert_eq!(HolderState::of(&h), HolderState::Parked(&bench()));
    }

    #[test]
    fn a_frozen_holder_is_frozen_over_preparing() {
        let mut h = holder(Mode::Shared);
        h.ahead = Some(PreemptionState::Paused(bench()));
        h.prepare = Some(String::new());
        assert_eq!(HolderState::of(&h), HolderState::Frozen(&bench()));
    }

    #[test]
    fn a_running_preemptible_holder_is_not_frozen() {
        let mut h = holder(Mode::Shared);
        h.ahead = Some(PreemptionState::Running);
        assert_eq!(HolderState::of(&h), HolderState::Unestimated);
    }

    #[test]
    fn a_preparing_holder_is_preparing_over_idle() {
        let mut h = holder(Mode::Shared);
        h.prepare = Some("copying".into());
        h.idle = Some(idleness());
        assert_eq!(HolderState::of(&h), HolderState::Preparing("copying"));
    }

    #[test]
    fn an_idle_holder_is_idle_over_silent() {
        let mut h = holder(Mode::Shared);
        h.idle = Some(idleness());
        h.silent = Some(silence());
        assert_eq!(HolderState::of(&h), HolderState::Idle(&idleness()));
    }

    #[test]
    fn a_silent_holder_is_silent_over_overrun() {
        let mut h = holder(Mode::Shared);
        h.silent = Some(silence());
        h.estimate = Some(estimate(true));
        assert_eq!(HolderState::of(&h), HolderState::Silent(&silence()));
    }

    #[test]
    fn an_overrun_holder_says_its_overrun() {
        let mut h = holder(Mode::Shared);
        h.estimate = Some(estimate(true));
        assert_eq!(
            HolderState::of(&h),
            HolderState::Overrun(Overrun { high_secs: 60 }, &estimate(true))
        );
    }

    #[test]
    fn a_holder_within_its_history_is_estimated() {
        let mut h = holder(Mode::Shared);
        h.estimate = Some(estimate(false));
        assert_eq!(
            HolderState::of(&h),
            HolderState::Estimated(&estimate(false))
        );
    }

    #[test]
    fn an_overrun_estimate_says_its_ninetieth_percentile() {
        assert_eq!(
            estimate(true).overrun().map(|o| o.to_text()).as_deref(),
            Some("over 2x its p90, 1m00s")
        );
    }

    #[test]
    fn an_estimate_within_its_history_is_no_overrun() {
        assert_eq!(estimate(false).overrun(), None);
    }

    #[test]
    fn a_silence_says_how_long_its_output_has_not_changed() {
        let silent = Silence {
            silent_for_secs: 3000,
            silent_line: String::new(),
        };
        assert_eq!(silent.to_text(), "no output in 50m00s");
    }

    #[test]
    fn an_idle_wait_knowing_nothing_says_only_what_it_waits_for() {
        assert_eq!(wait().to_text(), "waits for an idle machine");
    }

    #[test]
    fn an_idle_wait_behind_a_job_names_it_over_the_quiet() {
        let behind = IdleWait {
            behind: Some("build from a".into()),
            quiet_for: Some(30),
            ..wait()
        };
        assert_eq!(
            behind.to_text(),
            "waits for an idle machine, behind build from a"
        );
    }

    #[test]
    fn an_idle_wait_on_a_quiet_machine_says_how_long_of_its_settle() {
        let quiet = IdleWait {
            quiet_for: Some(30),
            ..wait()
        };
        assert_eq!(
            quiet.to_text(),
            "waits for an idle machine, quiet 30s of 2m00s"
        );
    }

    #[test]
    fn a_parked_idle_wait_says_what_it_ran_and_that_it_runs_again() {
        let stopped = IdleWait {
            stopped: Some(LowPriorityStop {
                ran_secs: 300,
                ago_secs: 60,
                by: "bench from b".into(),
            }),
            ..wait()
        };
        assert_eq!(
            stopped.to_text(),
            "waits for an idle machine; ran 5m00s until bench from b came 1m00s ago, and runs again from the start"
        );
    }
}
