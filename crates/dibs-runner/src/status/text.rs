use crate::status::scene::{Leftover, Listing, Orphan, Scene, Service};
use dibs_format::{
    Mode, Span,
    status::{
        BatchShown, BatchTimeLeft, Closure, Holder, HolderState, IdleKind, Idleness, JobCall,
        LockState, RemainingBasis, Scope, ShownEstimate, Silence, Status, Waiter,
    },
};

/// An orphan's or a leftover's description is cut to this many characters, its indent included.
const DESCRIBED_CHARS: usize = 100;
/// Hues an agent's name is drawn in. Reds, greens and yellows are left out: they mean state here.
const HUES: [u8; 12] = [33, 39, 63, 99, 105, 135, 170, 176, 205, 38, 44, 111];

/// What one look at the lock found: the document `dibs status --json` writes, and what only its
/// text shows.
pub struct Snapshot {
    pub status: Status,
    pub scene: Scene,
}

impl Snapshot {
    /// The status as a person reads it, coloured for a terminal when `colour` is set.
    pub fn to_text(&self, colour: bool) -> String {
        let p = Palette::with_colour(colour);
        let (status, scene) = (&self.status, &self.scene);
        let mut out = String::new();
        write_listing(&mut out, scene.listing.as_ref());
        write_closure(&mut out, &p, status);
        out.push_str(&head(&p, status, scene));
        for holder in &status.holders {
            write_holder(&mut out, &p, holder, scene);
        }
        for waiter in &status.queue {
            write_waiter(&mut out, &p, waiter, status.queue.len());
        }
        write_arrival_note(&mut out, &p, status.queue.len());
        write_leftovers(&mut out, &p, &scene.leftovers);
        out
    }
}

/// The escapes, empty when the text is not for a terminal.
struct Palette {
    pub off: &'static str,
    pub dim: &'static str,
    pub bold: &'static str,
    pub busy: &'static str,
    pub free: &'static str,
    pub warn: &'static str,
    pub queued: &'static str,
    pub bench: &'static str,
    pub shared: &'static str,
    pub agents: bool,
}

impl Palette {
    fn with_colour(colour: bool) -> Palette {
        match colour {
            true => Palette {
                off: "\x1b[0m",
                dim: "\x1b[2m",
                bold: "\x1b[1m",
                busy: "\x1b[1;31m",
                free: "\x1b[1;32m",
                warn: "\x1b[33m",
                queued: "\x1b[36m",
                bench: "\x1b[1;31m",
                shared: "\x1b[1;33m",
                agents: true,
            },
            false => Palette {
                off: "",
                dim: "",
                bold: "",
                busy: "",
                free: "",
                warn: "",
                queued: "",
                bench: "",
                shared: "",
                agents: false,
            },
        }
    }

    /// The modes take the colours the first line gives them, so a row reads like its header.
    fn mode(&self, mode: Mode) -> &'static str {
        match mode {
            Mode::Bench => self.bench,
            Mode::Rsh => self.queued,
            _ => self.shared,
        }
    }

    /// The escape an agent's name is drawn in.
    fn agent(&self, name: &str) -> String {
        match self.agents {
            true => format!("\x1b[38;5;{}m", hue_of(name)),
            false => String::new(),
        }
    }
}

/// One hue per agent, so a session is the same colour wherever it appears.
fn hue_of(name: &str) -> u8 {
    let hash = name.chars().fold(7u32, |h, c| {
        (h.wrapping_mul(31).wrapping_add(c as u32)) & 0xffff
    });
    HUES[hash as usize % HUES.len()]
}

/// What a holder's line says after its lead, and which of the lines under it are written.
struct HolderVerdict {
    /// The bracket, coloured.
    pub said: String,
    /// It may be stuck, so how to stop it follows.
    pub stop: bool,
    /// Its `--with` servers are written.
    pub services: bool,
    /// The file it writes to is written.
    pub output: bool,
    /// The last line it wrote is written.
    pub last_line: bool,
    /// Its batch's time left is estimated, which a holder's that waits, idles or prepares is not.
    pub estimated: bool,
}

impl HolderVerdict {
    /// In this view's words, what `HolderState` says comes first.
    fn of(p: &Palette, holder: &Holder) -> HolderVerdict {
        match HolderState::of(holder) {
            HolderState::UrgentAlone(why) => {
                HolderVerdict::waiting(format!("{}[URGENT: {why}]{}", p.warn, p.off))
            }
            HolderState::Parked(urgent) => HolderVerdict::waiting(format!(
                "{}[parked for the urgent job {urgent}, and runs again from the start after it]{}",
                p.dim, p.off
            )),
            HolderState::Frozen(bench) => HolderVerdict::waiting(format!(
                "{}[frozen while the benchmark {bench} runs, which was queued first]{}",
                p.dim, p.off
            )),
            HolderState::Preparing(doing) => HolderVerdict::preparing(format!(
                "{}[preparing its tree, before the command: {doing}]{}",
                p.dim, p.off
            )),
            HolderState::Idle(idle) => HolderVerdict::idle(format!(
                "{}[IDLE: {}, it is waiting on something]{}",
                p.warn,
                idleness_said(holder, idle),
                p.off
            )),
            HolderState::Silent(silent) => {
                HolderVerdict::stuck(format!("{}[STUCK? {}]{}", p.warn, silent.to_text(), p.off))
            }
            HolderState::Overrun(overrun, _) => {
                HolderVerdict::stuck(format!("{}[STUCK? {}]{}", p.warn, overrun.to_text(), p.off))
            }
            HolderState::Estimated(shown) => HolderVerdict::timed(format!(
                "{}[{}]{}",
                p.dim,
                estimate_said(shown, holder.call.mode),
                p.off
            )),
            HolderState::Unestimated => {
                HolderVerdict::timed(format!("{}[no history for this one yet]{}", p.dim, p.off))
            }
        }
    }

    /// Waiting, or urgent with the machine to itself: none of its time is its own history's.
    fn waiting(said: String) -> HolderVerdict {
        HolderVerdict {
            said,
            stop: false,
            services: false,
            output: false,
            last_line: false,
            estimated: false,
        }
    }

    fn preparing(said: String) -> HolderVerdict {
        HolderVerdict {
            said,
            stop: false,
            services: true,
            output: true,
            last_line: true,
            estimated: false,
        }
    }

    fn idle(said: String) -> HolderVerdict {
        HolderVerdict {
            said,
            stop: true,
            services: true,
            output: false,
            last_line: true,
            estimated: false,
        }
    }

    fn stuck(said: String) -> HolderVerdict {
        HolderVerdict {
            said,
            stop: true,
            services: true,
            output: true,
            last_line: true,
            estimated: true,
        }
    }

    fn timed(said: String) -> HolderVerdict {
        HolderVerdict {
            said,
            stop: false,
            services: true,
            output: true,
            last_line: true,
            estimated: true,
        }
    }
}

/// The lock directory's entries and the history's size, for a caller that asked with `-v`.
fn write_listing(out: &mut String, listing: Option<&Listing>) {
    if let Some(listing) = listing {
        out.push_str(&listing_text(listing));
    }
}

fn listing_text(listing: &Listing) -> String {
    let entries: String = listing
        .entries
        .iter()
        .map(|entry| format!("  {entry}\n"))
        .collect();
    format!(
        "  [{}]\n{entries}  [{}: {} runs recorded]\n",
        listing.dir.display(),
        listing.history.display(),
        listing.run_count
    )
}

fn write_closure(out: &mut String, p: &Palette, status: &Status) {
    if let Some(closed) = &status.closed {
        out.push_str(&closure_text(p, closed, status.read_secs));
    }
}

fn closure_text(p: &Palette, closed: &Closure, read_secs: u64) -> String {
    let reason = closed
        .close
        .reason
        .as_ref()
        .map(|reason| format!("  {reason}\n"))
        .unwrap_or_default();
    let except = match closed.close.except.as_slice() {
        [] => String::new(),
        except => format!("  {} may still run work here.\n", except.join(", ")),
    };
    format!(
        "{}dibs: CLOSED by {}{}, {} ago: nothing new starts until it is opened.\n{reason}{except}",
        p.busy,
        closed.by,
        p.off,
        Span::from_secs(read_secs.saturating_sub(closed.since_secs))
    )
}

/// The line that says who holds the lock, and the lines under it when no record does.
fn head(p: &Palette, status: &Status, scene: &Scene) -> String {
    let queued = match status.queue.len() {
        0 => String::new(),
        n => format!("{} ({n} queued){}", p.dim, p.off),
    };
    match (status.holders.is_empty(), status.state) {
        (false, LockState::Bench) => format!(
            "{}dibs: BUSY, benchmark in progress{}{queued}\n",
            p.busy, p.off
        ),
        (false, _) => format!("{}dibs: in use, shared{}{queued}\n", p.warn, p.off),
        (true, state) => unheld(p, state, scene),
    }
}

/// What is said when no record holds the lock. A state this dibs does not know is said as idle,
/// as a shared or a benchmark state is, which a runner writes only beside a holder.
fn unheld(p: &Palette, state: LockState, scene: &Scene) -> String {
    match (state, scene.unseen) {
        (LockState::Orphan, _) => orphaned(p, &scene.orphans),
        (LockState::Busy, true) => format!(
            "{}dibs: the lock is taken and nothing here reports holding it.{}\n  Whatever holds it is out of this account's sight, so it cannot be named.\n",
            p.busy, p.off
        ),
        (LockState::Busy, false) => format!(
            "{}dibs: busy{}, a client has just taken the lock and is recording it.\n",
            p.busy, p.off
        ),
        (LockState::Idle | LockState::Shared | LockState::Bench | LockState::Other, _) => {
            format!("{}dibs: idle{}\n", p.free, p.off)
        }
    }
}

fn orphaned(p: &Palette, orphans: &[Orphan]) -> String {
    let holding: String = orphans
        .iter()
        .map(|orphan| format!("{}\n", described(&orphan.described)))
        .collect();
    format!(
        "{}dibs: LOCKED BY AN ORPHAN.{} No holder record, but the lock is taken, so\n  something outlived its parent. Nothing can run until it goes.\n  holding it:\n{holding}  Stop it with: dibs --kill <pid> --anyone\n",
        p.busy, p.off
    )
}

/// A process's description under its heading, cut to `DESCRIBED_CHARS`.
fn described(described: &str) -> String {
    format!("    {described}")
        .chars()
        .take(DESCRIBED_CHARS)
        .collect()
}

fn write_holder(out: &mut String, p: &Palette, holder: &Holder, scene: &Scene) {
    let verdict = HolderVerdict::of(p, holder);
    out.push_str(&format!("{}   {}\n", lead(p, holder), verdict.said));
    write_stop(out, p, holder.call.pid, verdict.stop);
    let services: &[Service] = match verdict.services {
        true => scene.services_of(holder.call.pid),
        false => &[],
    };
    write_call(
        out,
        p,
        &holder.call,
        services,
        holder.batch.as_ref(),
        verdict.estimated,
    );
    write_output(out, p, holder, &scene.host, verdict.output);
    write_last_line(out, p, holder.silent.as_ref(), verdict.last_line);
}

/// What every holder's line starts with: its mode, label, time, cap, pid and job, and what sets
/// it apart from the others.
fn lead(p: &Palette, holder: &Holder) -> String {
    let call = &holder.call;
    let cap = holder
        .killed_at
        .map(|at| format!(", {}cap {}{}", p.dim, Span::from_secs(at), p.off))
        .unwrap_or_default();
    let job = holder
        .job
        .as_ref()
        .map(|job| format!("  {}job {job}{}", p.dim, p.off))
        .unwrap_or_default();
    let preemptible = holder
        .ahead
        .as_ref()
        .map(|_| format!("  {}preemptible{}", p.dim, p.off))
        .unwrap_or_default();
    let low = match holder.low_priority {
        true => format!("  {}low priority{}", p.dim, p.off),
        false => String::new(),
    };
    let urgent = holder
        .urgent
        .as_ref()
        .filter(|_| call.mode == Mode::Shared)
        .map(|why| format!("  {}urgent: {why}{}", p.warn, p.off))
        .unwrap_or_default();
    format!(
        "  {}{}{}  {}{}{}  {}{}{}{cap}  {}pid {}{}{job}{preemptible}{low}{urgent}",
        p.mode(call.mode),
        call.mode,
        p.off,
        p.agent(&call.agent),
        call.label,
        p.off,
        p.bold,
        Span::from_secs(holder.elapsed_secs),
        p.off,
        p.dim,
        call.pid,
        p.off
    )
}

fn idleness_said(holder: &Holder, idle: &Idleness) -> String {
    match idle.idle_kind {
        IdleKind::Never => format!("no CPU at all in {}", Span::from_secs(idle.idle_for_secs)),
        IdleKind::Stalled => format!(
            "{}s of CPU, none of it in the last {}",
            holder.cpu_secs,
            Span::from_secs(idle.idle_for_secs)
        ),
    }
}

fn write_stop(out: &mut String, p: &Palette, pid: u32, stop: bool) {
    if stop {
        out.push_str(&format!(
            "    {}stop it with: dibs --kill {pid}{}\n",
            p.warn, p.off
        ));
    }
}

/// The lines under every job's: its command, its servers, who it is from, and its batch.
fn write_call(
    out: &mut String,
    p: &Palette,
    call: &JobCall,
    services: &[Service],
    batch: Option<&BatchShown>,
    estimated: bool,
) {
    out.push_str(&format!("    {}{}{}\n", p.dim, call.cmd, p.off));
    for service in services {
        out.push_str(&format!(
            "    {}with {}, pid {}:{} {}{}{}\n",
            p.dim, service.name, service.pid, p.off, p.dim, service.command, p.off
        ));
    }
    out.push_str(&from_line(p, call));
    if let Some(batch) = batch {
        out.push_str(&batch_lines(p, batch, estimated));
    }
}

fn from_line(p: &Palette, call: &JobCall) -> String {
    let user = call
        .user
        .as_ref()
        .map(|user| format!(" {}({user}){}", p.dim, p.off))
        .unwrap_or_default();
    let device = call
        .device
        .as_ref()
        .map(|device| format!("{} on {}{device}", p.dim, p.off))
        .unwrap_or_default();
    format!(
        "    {}from{} {}{}{}{user}{device}\n",
        p.dim,
        p.off,
        p.agent(&call.agent),
        call.agent,
        p.off
    )
}

fn write_output(out: &mut String, p: &Palette, holder: &Holder, host: &str, shown: bool) {
    if let Some(output) = holder.output.as_ref().filter(|_| shown) {
        out.push_str(&format!(
            "    {}writing {output}{}   {}(dibs --on {host} --out {}){}\n",
            p.dim, p.off, p.dim, holder.call.pid, p.off
        ));
    }
}

fn write_last_line(out: &mut String, p: &Palette, silent: Option<&Silence>, shown: bool) {
    if let Some(silent) = silent.filter(|silent| shown && !silent.silent_line.is_empty()) {
        out.push_str(&format!(
            "    {}its last line, {} ago:{} {}\n",
            p.warn,
            Span::from_secs(silent.silent_for_secs),
            p.off,
            silent.silent_line
        ));
    }
}

/// The batch's lines under a job, its time left said only where it is `estimated`.
fn batch_lines(p: &Palette, batch: &BatchShown, estimated: bool) -> String {
    let within = batch
        .within
        .map(|place| format!(", job {} of {}", place.job, place.job_count))
        .unwrap_or_default();
    let then = match batch.here_count {
        0 => String::new(),
        _ => format!("    {}then here:{} {}\n", p.dim, p.off, batch.next),
    };
    format!(
        "    {}batch{} {}{}, step {} of {}{within}: {}{}\n{then}    {}batch time left here: {}{}\n",
        p.dim,
        p.off,
        batch.id,
        p.dim,
        batch.k,
        batch.step_count,
        batch.step,
        p.off,
        p.dim,
        batch_left_said(batch.left.as_ref().filter(|_| estimated)),
        p.off
    )
}

fn batch_left_said(left: Option<&BatchTimeLeft>) -> String {
    match left {
        None => "unknown, this step's own time cannot be estimated".to_string(),
        Some(left) if left.left_partial => format!(
            "over {}, since some of what is ahead has no history",
            Span::from_secs(left.left_secs)
        ),
        Some(left) => format!("~{}", Span::from_secs(left.left_secs)),
    }
}

fn write_waiter(out: &mut String, p: &Palette, waiter: &Waiter, waiter_count: usize) {
    let call = &waiter.call;
    let urgent = waiter
        .urgent
        .as_ref()
        .map(|why| format!("   {}[URGENT: {why}]{}", p.warn, p.off))
        .unwrap_or_default();
    out.push_str(&format!(
        "  {}queued {} of {waiter_count}{}: {}{}{}  {}{}{}  waiting {}{}{}  {}pid {}{}{urgent}{}\n",
        p.queued,
        waiter.position,
        p.off,
        p.mode(call.mode),
        call.mode,
        p.off,
        p.agent(&call.agent),
        call.label,
        p.off,
        p.bold,
        Span::from_secs(waiter.waiting_secs),
        p.off,
        p.dim,
        call.pid,
        p.off,
        starts(p, waiter)
    ));
    write_call(out, p, call, &[], waiter.batch.as_ref(), true);
}

/// When a waiter should start, or what it waits for instead.
fn starts(p: &Palette, waiter: &Waiter) -> String {
    match (&waiter.idle_wait, waiter.eta) {
        (Some(wait), _) => format!("   {}[{}]{}", p.dim, wait.to_text(), p.off),
        (None, Some(0)) => format!("   {}[starts as soon as the lock frees]{}", p.dim, p.off),
        (None, Some(eta)) => format!(
            "   {}[~{} until it starts]{}",
            p.dim,
            Span::from_secs(eta),
            p.off
        ),
        (None, None) => String::new(),
    }
}

fn write_arrival_note(out: &mut String, p: &Palette, waiter_count: usize) {
    if waiter_count > 1 {
        out.push_str(&format!(
            "  {}(queued in arrival order; the kernel picks the actual wake order){}\n",
            p.dim, p.off
        ));
    }
}

/// What outlived its job, which holds no lock and so is in no record: it runs beside whatever
/// is measured next.
fn write_leftovers(out: &mut String, p: &Palette, leftovers: &[Leftover]) {
    if !leftovers.is_empty() {
        out.push_str(&left_running(p, leftovers));
    }
}

fn left_running(p: &Palette, leftovers: &[Leftover]) -> String {
    let lines: String = leftovers
        .iter()
        .map(|left| {
            format!(
                "{}  (job {}, {})\n",
                described(&left.described),
                left.job,
                left.label
            )
        })
        .collect();
    format!(
        "{}dibs: LEFT RUNNING.{} These outlived the jobs that started them, and hold no lock:\n{lines}  Stop one with: dibs --kill <pid>\n",
        p.warn, p.off
    )
}

/// What the history says, then what that leaves: `usually 5m00s over 3 runs, ~2m00s left`.
fn estimate_said(shown: &ShownEstimate, mode: Mode) -> String {
    let usual = scoped(shown, mode, usual_said(shown));
    match left_said(shown) {
        Some(left) => format!("{usual}, {left}"),
        None => usual,
    }
}

/// What the runs took, or how far they spread.
fn usual_said(shown: &ShownEstimate) -> String {
    match (shown.wide, shown.median_secs, shown.run_count) {
        (true, ..) => format!(
            "anywhere from {} to {} over {} runs",
            low_secs_said(shown.low_secs),
            Span::from_secs(shown.high_secs),
            shown.run_count
        ),
        (false, 0, runs) => format!("under a second over {runs} runs"),
        (false, median, 1) => format!("ran once, in {}", Span::from_secs(median)),
        (false, median, runs) => format!("usually {} over {runs} runs", Span::from_secs(median)),
    }
}

fn low_secs_said(low_secs: u64) -> String {
    match low_secs {
        0 => "under a second".to_string(),
        low => Span::from_secs(low).to_string(),
    }
}

/// What the runs are of, where they are not this job's own.
fn scoped(shown: &ShownEstimate, mode: Mode, usual: String) -> String {
    match (shown.scope, shown.other) {
        (Scope::Agent, _) => {
            format!("nothing on this one; this agent's other {mode} jobs: {usual}")
        }
        (Scope::Mode, _) => {
            format!("nothing on this one; every {mode} job on the machine: {usual}")
        }
        (Scope::This, true) => format!("nothing with these values; with others: {usual}"),
        (Scope::This, false) => usual,
    }
}

/// What the estimate leaves, none for a job whose every run took under a second.
fn left_said(shown: &ShownEstimate) -> Option<String> {
    match (
        shown.wide,
        shown.median_secs,
        shown.remaining,
        shown.remaining_kind,
    ) {
        (false, 0, ..) => None,
        (_, _, Some(left), Some(RemainingBasis::Bound)) => Some(format!(
            "under {} left if it runs true to form",
            Span::from_secs(left)
        )),
        (_, _, Some(left), _) => Some(format!("~{} left", Span::from_secs(left))),
        (_, _, None, _) => Some("longer than it has ever taken".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dibs_format::{
        BatchId, JobId, Label,
        status::{IdleWait, PreemptionState, RecipePlace},
        wire::Close,
    };
    use std::path::PathBuf;

    fn status(state: LockState) -> Status {
        Status {
            read_secs: 0,
            state,
            core_count: 1,
            load_hundredths: 0,
            caches: Vec::new(),
            clones: Vec::new(),
            holders: Vec::new(),
            queue: Vec::new(),
            metrics: None,
            closed: None,
            series: Vec::new(),
        }
    }

    fn call(mode: Mode) -> JobCall {
        JobCall {
            mode,
            pid: 42,
            label: Label::new("build"),
            agent: "an agent".into(),
            user: None,
            subagent: None,
            device: None,
            cmd: "cargo build".into(),
            command: None,
        }
    }

    fn holder(mode: Mode) -> Holder {
        Holder {
            call: call(mode),
            job: None,
            started_secs: 0,
            elapsed_secs: 65,
            killed_at: None,
            cpu_secs: 12,
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

    fn waiter(eta: Option<u64>) -> Waiter {
        Waiter {
            position: 1,
            call: call(Mode::Shared),
            arrived_secs: 0,
            waiting_secs: 5,
            eta,
            low_priority: false,
            urgent: None,
            idle_wait: None,
            batch: None,
        }
    }

    fn estimate() -> ShownEstimate {
        ShownEstimate {
            median_secs: 300,
            low_secs: 200,
            high_secs: 400,
            run_count: 3,
            scope: Scope::This,
            wide: false,
            other: false,
            remaining: Some(120),
            remaining_kind: Some(RemainingBasis::Typical),
            overrun: false,
        }
    }

    fn batch() -> BatchShown {
        BatchShown {
            id: BatchId::new("20261001-1"),
            step: "build".into(),
            k: 1,
            step_count: 3,
            within: None,
            here_count: 0,
            next: String::new(),
            left: Some(BatchTimeLeft {
                left_secs: 600,
                left_partial: false,
            }),
        }
    }

    fn text_with(status: &Status, scene: &Scene) -> String {
        Snapshot {
            status: status.clone(),
            scene: scene.clone(),
        }
        .to_text(false)
    }

    fn text(status: &Status) -> String {
        text_with(status, &Scene::default())
    }

    fn held(holder: Holder) -> String {
        let mut s = status(LockState::Shared);
        s.holders.push(holder);
        text(&s)
    }

    fn queued(waiters: Vec<Waiter>) -> String {
        let mut s = status(LockState::Idle);
        s.queue = waiters;
        text(&s)
    }

    fn said(shown: ShownEstimate) -> String {
        estimate_said(&shown, Mode::Shared)
    }

    #[test]
    fn a_lock_taken_out_of_sight_says_it_cannot_be_named() {
        let unseen = Scene {
            unseen: true,
            ..Scene::default()
        };
        assert_eq!(
            text_with(&status(LockState::Busy), &unseen),
            "dibs: the lock is taken and nothing here reports holding it.\n  Whatever holds it is out of this account's sight, so it cannot be named.\n"
        );
    }

    #[test]
    fn a_lock_taken_in_sight_is_a_client_recording_itself() {
        assert_eq!(
            text(&status(LockState::Busy)),
            "dibs: busy, a client has just taken the lock and is recording it.\n"
        );
    }

    fn orphaned() -> String {
        let orphaned = Scene {
            orphans: vec![Orphan {
                pid: 9,
                described: "x".repeat(200),
            }],
            ..Scene::default()
        };
        text_with(&status(LockState::Orphan), &orphaned)
    }

    #[test]
    fn a_lock_an_orphan_holds_says_so() {
        assert!(orphaned().starts_with("dibs: LOCKED BY AN ORPHAN."));
    }

    #[test]
    fn an_orphan_is_named_cut_to_its_width() {
        assert!(orphaned().contains(&format!("\n    {}\n", "x".repeat(DESCRIBED_CHARS - 4))));
    }

    #[test]
    fn a_state_this_dibs_does_not_know_with_no_holder_reads_as_idle() {
        assert_eq!(text(&status(LockState::Other)), "dibs: idle\n");
    }

    #[test]
    fn a_benchmark_holding_says_the_machine_is_busy() {
        let mut s = status(LockState::Bench);
        s.holders.push(holder(Mode::Bench));
        assert!(text(&s).starts_with("dibs: BUSY, benchmark in progress\n"));
    }

    #[test]
    fn a_held_machine_says_how_many_wait() {
        let mut s = status(LockState::Shared);
        s.holders.push(holder(Mode::Shared));
        s.queue.push(waiter(None));
        assert!(text(&s).starts_with("dibs: in use, shared (1 queued)\n"));
    }

    #[test]
    fn a_listing_names_the_entries_and_the_runs_recorded() {
        let listed = Scene {
            listing: Some(Listing {
                dir: PathBuf::from("/lock"),
                entries: vec!["holder.1".into()],
                history: PathBuf::from("/history"),
                run_count: 4,
            }),
            ..Scene::default()
        };
        assert!(
            text_with(&status(LockState::Idle), &listed)
                .starts_with("  [/lock]\n  holder.1\n  [/history: 4 runs recorded]\n")
        );
    }

    fn closed(except: Vec<String>, reason: Option<&str>) -> String {
        let mut s = status(LockState::Idle);
        s.read_secs = 120;
        s.closed = Some(Closure {
            by: "owner".into(),
            since_secs: 60,
            close: Close {
                now: false,
                except,
                reason: reason.map(str::to_string),
            },
        });
        text(&s)
    }

    #[test]
    fn a_closed_machine_says_why() {
        assert!(
            closed(Vec::new(), Some("maintenance")).starts_with(
                "dibs: CLOSED by owner, 1m00s ago: nothing new starts until it is opened.\n  maintenance\ndibs: idle\n"
            )
        );
    }

    #[test]
    fn a_closed_machine_says_who_may_still_run() {
        assert!(closed(vec!["a".into(), "b".into()], None).starts_with(
            "dibs: CLOSED by owner, 1m00s ago: nothing new starts until it is opened.\n  a, b may still run work here.\ndibs: idle\n"
        ));
    }

    #[test]
    fn a_closed_machine_with_no_reason_or_exception_says_only_that() {
        assert_eq!(
            closed(Vec::new(), None),
            "dibs: CLOSED by owner, 1m00s ago: nothing new starts until it is opened.\ndibs: idle\n"
        );
    }

    #[test]
    fn a_holder_with_no_history_says_so_under_its_lead() {
        assert_eq!(
            held(holder(Mode::Shared)),
            "dibs: in use, shared\n  shared  build  1m05s  pid 42   [no history for this one yet]\n    cargo build\n    from an agent\n"
        );
    }

    #[test]
    fn a_holder_with_a_job_names_it_beside_its_pid() {
        let mut h = holder(Mode::Shared);
        h.job = Some(JobId::new("20261001120000-42"));
        assert!(held(h).contains("  pid 42  job 20261001120000-42   [no history"));
    }

    #[test]
    fn a_capped_holder_says_its_cap_beside_its_time() {
        let mut h = holder(Mode::Shared);
        h.killed_at = Some(1800);
        assert!(held(h).contains("  shared  build  1m05s, cap 30m00s  pid 42"));
    }

    #[test]
    fn a_preemptible_holder_says_so_in_line() {
        let mut h = holder(Mode::Shared);
        h.ahead = Some(PreemptionState::Running);
        assert!(held(h).contains("pid 42  preemptible   [no history"));
    }

    #[test]
    fn a_low_priority_holder_says_so_in_line() {
        let mut h = holder(Mode::Shared);
        h.low_priority = true;
        assert!(held(h).contains("pid 42  low priority   [no history"));
    }

    #[test]
    fn an_urgent_shared_holder_says_why_in_line() {
        let mut h = holder(Mode::Shared);
        h.urgent = Some("demo".into());
        assert!(held(h).contains("pid 42  urgent: demo   [no history"));
    }

    #[test]
    fn an_urgent_benchmark_says_why_and_nothing_of_its_history() {
        let mut h = holder(Mode::Bench);
        h.urgent = Some("demo".into());
        h.estimate = Some(estimate());
        assert!(held(h).contains("pid 42   [URGENT: demo]\n    cargo build\n    from an agent\n"));
    }

    #[test]
    fn a_parked_holder_says_what_it_waits_for() {
        let mut h = holder(Mode::Shared);
        h.parked = Some(JobId::new("20261001120000-9"));
        assert!(held(h).contains(
            "   [parked for the urgent job 20261001120000-9, and runs again from the start after it]\n"
        ));
    }

    #[test]
    fn a_frozen_holder_says_which_benchmark_it_waits_for() {
        let mut h = holder(Mode::Shared);
        h.ahead = Some(PreemptionState::Paused(JobId::new("20261001120000-9")));
        assert!(held(h).contains(
            "   [frozen while the benchmark 20261001120000-9 runs, which was queued first]\n    cargo build\n"
        ));
    }

    fn parked() -> Holder {
        let mut h = holder(Mode::Shared);
        h.parked = Some(JobId::new("20261001120000-9"));
        h
    }

    /// A text with the holder running one `--with` server.
    fn served(holder: Holder) -> String {
        let services = Scene {
            services: [(
                42,
                vec![Service {
                    name: "srv".into(),
                    pid: 43,
                    command: "serve".into(),
                }],
            )]
            .into(),
            ..Scene::default()
        };
        let mut s = status(LockState::Shared);
        s.holders.push(holder);
        text_with(&s, &services)
    }

    #[test]
    fn a_waiting_holder_estimates_no_batch_time() {
        let mut h = parked();
        h.batch = Some(batch());
        assert!(
            held(h).contains(
                "batch time left here: unknown, this step's own time cannot be estimated\n"
            )
        );
    }

    #[test]
    fn a_waiting_holder_names_no_servers() {
        assert!(!served(parked()).contains("with srv"));
    }

    #[test]
    fn a_holder_names_its_servers_under_its_command() {
        assert!(
            served(holder(Mode::Shared))
                .contains("    cargo build\n    with srv, pid 43: serve\n    from an agent\n")
        );
    }

    #[test]
    fn a_preparing_holder_says_what_the_machine_said_last() {
        let mut h = holder(Mode::Shared);
        h.prepare = Some("copying a-target's target".into());
        h.estimate = Some(estimate());
        assert!(held(h).contains(
            "pid 42   [preparing its tree, before the command: copying a-target's target]\n"
        ));
    }

    #[test]
    fn an_idle_holder_that_never_worked_says_so_and_how_to_stop_it() {
        let mut h = holder(Mode::Shared);
        h.idle = Some(Idleness {
            idle_for_secs: 600,
            idle_kind: IdleKind::Never,
        });
        assert!(held(h).contains(
            "pid 42   [IDLE: no CPU at all in 10m00s, it is waiting on something]\n    stop it with: dibs --kill 42\n    cargo build\n"
        ));
    }

    #[test]
    fn an_idle_holder_that_stalled_says_what_it_used_before() {
        let mut h = holder(Mode::Shared);
        h.idle = Some(Idleness {
            idle_for_secs: 600,
            idle_kind: IdleKind::Stalled,
        });
        assert!(held(h).contains("[IDLE: 12s of CPU, none of it in the last 10m00s,"));
    }

    #[test]
    fn an_idle_holder_names_no_output_file() {
        let mut h = holder(Mode::Shared);
        h.output = Some("/out/log".into());
        h.idle = Some(Idleness {
            idle_for_secs: 600,
            idle_kind: IdleKind::Never,
        });
        assert!(!held(h).contains("writing /out/log"));
    }

    fn silent() -> Holder {
        let mut h = holder(Mode::Shared);
        h.silent = Some(Silence {
            silent_for_secs: 3000,
            silent_line: "test all_reduce has been running for over 60 seconds".into(),
        });
        h
    }

    #[test]
    fn a_silent_holder_asks_whether_it_is_stuck() {
        assert!(
            held(silent()).contains(
                "pid 42   [STUCK? no output in 50m00s]\n    stop it with: dibs --kill 42\n"
            )
        );
    }

    #[test]
    fn a_silent_holder_shows_its_last_line() {
        assert!(held(silent()).ends_with(
            "    its last line, 50m00s ago: test all_reduce has been running for over 60 seconds\n"
        ));
    }

    #[test]
    fn a_silent_holder_that_wrote_no_line_shows_none() {
        let mut h = holder(Mode::Shared);
        h.silent = Some(Silence {
            silent_for_secs: 3000,
            silent_line: String::new(),
        });
        assert!(!held(h).contains("its last line"));
    }

    #[test]
    fn an_overrun_holder_asks_whether_it_is_stuck() {
        let mut h = holder(Mode::Shared);
        h.estimate = Some(ShownEstimate {
            overrun: true,
            ..estimate()
        });
        assert!(held(h).contains(
            "pid 42   [STUCK? over 2x its p90, 6m40s]\n    stop it with: dibs --kill 42\n"
        ));
    }

    #[test]
    fn a_holder_writing_a_file_says_how_to_read_it() {
        let mut h = holder(Mode::Shared);
        h.output = Some("/out/log".into());
        let mut s = status(LockState::Shared);
        s.holders.push(h);
        let here = Scene {
            host: "box".into(),
            ..Scene::default()
        };
        assert!(
            text_with(&s, &here).ends_with("    writing /out/log   (dibs --on box --out 42)\n")
        );
    }

    #[test]
    fn a_holder_names_the_person_and_the_card_it_runs_as_and_on() {
        let mut h = holder(Mode::Shared);
        h.call.user = Some("someone".into());
        h.call.device = Some(dibs_format::Alias::new("gpu:a"));
        assert!(held(h).contains("    from an agent (someone) on gpu:a\n"));
    }

    #[test]
    fn a_batch_says_its_place_what_comes_next_and_a_floor_on_its_time() {
        let mut h = holder(Mode::Shared);
        h.batch = Some(BatchShown {
            within: Some(RecipePlace {
                job: 2,
                job_count: 4,
            }),
            here_count: 1,
            next: "bench ~4m00s".into(),
            left: Some(BatchTimeLeft {
                left_secs: 620,
                left_partial: true,
            }),
            ..batch()
        });
        assert!(held(h).ends_with(
            "    batch 20261001-1, step 1 of 3, job 2 of 4: build\n    then here: bench ~4m00s\n    batch time left here: over 10m20s, since some of what is ahead has no history\n"
        ));
    }

    const UNESTIMATED: &str =
        "    batch time left here: unknown, this step's own time cannot be estimated\n";

    #[test]
    fn a_preparing_holders_batch_time_is_not_estimated() {
        let mut h = holder(Mode::Shared);
        h.prepare = Some(String::new());
        h.batch = Some(batch());
        assert!(held(h).ends_with(UNESTIMATED));
    }

    #[test]
    fn an_idle_holders_batch_time_is_not_estimated() {
        let mut h = holder(Mode::Shared);
        h.idle = Some(Idleness {
            idle_for_secs: 600,
            idle_kind: IdleKind::Never,
        });
        h.batch = Some(batch());
        assert!(held(h).ends_with(UNESTIMATED));
    }

    #[test]
    fn a_batch_known_in_full_says_its_time_left() {
        let mut h = holder(Mode::Shared);
        h.batch = Some(batch());
        assert!(held(h).ends_with("    batch time left here: ~10m00s\n"));
    }

    #[test]
    fn a_low_priority_waiter_says_what_it_waits_for() {
        let mut w = waiter(None);
        w.low_priority = true;
        w.idle_wait = Some(IdleWait {
            behind: None,
            quiet_for: None,
            settle_secs: 60,
            stopped: None,
        });
        assert!(queued(vec![w]).contains("pid 42   [waits for an idle machine]\n"));
    }

    #[test]
    fn a_waiter_next_in_line_starts_as_the_lock_frees() {
        assert!(
            queued(vec![waiter(Some(0))])
                .contains("  queued 1 of 1: shared  build  waiting 5s  pid 42   [starts as soon as the lock frees]\n")
        );
    }

    #[test]
    fn a_waiter_says_how_long_until_it_starts() {
        assert!(queued(vec![waiter(Some(90))]).contains("pid 42   [~1m30s until it starts]\n"));
    }

    #[test]
    fn a_waiter_with_no_estimate_says_nothing_of_when() {
        assert!(queued(vec![waiter(None)]).contains("pid 42\n    cargo build\n"));
    }

    #[test]
    fn an_urgent_waiter_says_why_before_when() {
        let mut w = waiter(Some(0));
        w.urgent = Some("release".into());
        assert!(
            queued(vec![w])
                .contains("pid 42   [URGENT: release]   [starts as soon as the lock frees]\n")
        );
    }

    #[test]
    fn a_waiter_estimates_its_batch_time() {
        let mut w = waiter(None);
        w.batch = Some(batch());
        assert!(queued(vec![w]).ends_with("    batch time left here: ~10m00s\n"));
    }

    #[test]
    fn two_waiters_are_said_to_be_in_arrival_order() {
        let said = queued(vec![waiter(None), waiter(None)]);
        assert!(
            said.ends_with("  (queued in arrival order; the kernel picks the actual wake order)\n")
        );
    }

    #[test]
    fn one_waiter_is_not_said_to_be_in_arrival_order() {
        assert!(!queued(vec![waiter(None)]).contains("arrival order"));
    }

    #[test]
    fn a_leftover_is_named_with_its_job_and_how_to_stop_it() {
        let left = Scene {
            leftovers: vec![Leftover {
                job: JobId::new("20261001120000-42"),
                label: Label::new("build"),
                described: "42 1:00 me cargo".into(),
            }],
            ..Scene::default()
        };
        assert!(text_with(&status(LockState::Idle), &left).ends_with(
            "hold no lock:\n    42 1:00 me cargo  (job 20261001120000-42, build)\n  Stop one with: dibs --kill <pid>\n"
        ));
    }

    #[test]
    fn an_estimate_short_of_its_median_says_the_time_left() {
        assert_eq!(said(estimate()), "usually 5m00s over 3 runs, ~2m00s left");
    }

    #[test]
    fn an_estimate_past_its_median_bounds_the_time_left() {
        let past = ShownEstimate {
            remaining_kind: Some(RemainingBasis::Bound),
            ..estimate()
        };
        assert_eq!(
            said(past),
            "usually 5m00s over 3 runs, under 2m00s left if it runs true to form"
        );
    }

    #[test]
    fn an_estimate_past_its_slowest_says_so() {
        let past = ShownEstimate {
            remaining: None,
            ..estimate()
        };
        assert_eq!(
            said(past),
            "usually 5m00s over 3 runs, longer than it has ever taken"
        );
    }

    #[test]
    fn runs_under_a_second_say_no_time_left() {
        let instant = ShownEstimate {
            median_secs: 0,
            ..estimate()
        };
        assert_eq!(said(instant), "under a second over 3 runs");
    }

    #[test]
    fn one_run_is_not_called_usual() {
        let once = ShownEstimate {
            run_count: 1,
            ..estimate()
        };
        assert_eq!(said(once), "ran once, in 5m00s, ~2m00s left");
    }

    #[test]
    fn runs_spread_wide_say_their_range() {
        let wide = ShownEstimate {
            wide: true,
            median_secs: 0,
            low_secs: 0,
            ..estimate()
        };
        assert_eq!(
            said(wide),
            "anywhere from under a second to 6m40s over 3 runs, ~2m00s left"
        );
    }

    #[test]
    fn runs_spread_wide_from_a_second_up_say_their_low() {
        let wide = ShownEstimate {
            wide: true,
            ..estimate()
        };
        assert!(said(wide).starts_with("anywhere from 3m20s to 6m40s"));
    }

    #[test]
    fn an_agents_other_jobs_are_said_to_be_its() {
        let agent = ShownEstimate {
            scope: Scope::Agent,
            ..estimate()
        };
        assert!(said(agent).starts_with("nothing on this one; this agent's other shared jobs: "));
    }

    #[test]
    fn every_job_of_a_mode_is_said_to_be_the_machines() {
        let mode = ShownEstimate {
            scope: Scope::Mode,
            ..estimate()
        };
        assert!(said(mode).starts_with("nothing on this one; every shared job on the machine: "));
    }

    #[test]
    fn runs_with_other_values_are_said_to_be() {
        let other = ShownEstimate {
            other: true,
            ..estimate()
        };
        assert!(said(other).starts_with("nothing with these values; with others: "));
    }

    #[test]
    fn an_agent_is_drawn_in_one_of_the_hues() {
        let escape = Palette::with_colour(true).agent("session a");
        assert!(
            HUES.iter().any(|hue| escape == format!("\x1b[38;5;{hue}m")),
            "{escape}"
        );
    }

    #[test]
    fn an_agent_is_drawn_plain_without_colour() {
        assert!(Palette::with_colour(false).agent("session a").is_empty());
    }
}
