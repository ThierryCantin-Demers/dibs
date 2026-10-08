use crate::{
    Alias, Mode, Span,
    status::base::{
        BatchShown, Holder, IdleKind, LockState, Remaining, Scope, Service, Shown, Status, Waiter,
    },
};
use std::fmt::{self, Write as _};

/// An orphan's description is cut to this many characters, its indent included.
const DESCRIBED: usize = 100;
/// Hues an agent's name is drawn in. Reds, greens and yellows are left out: they mean state here.
const HUES: [u8; 12] = [33, 39, 63, 99, 105, 135, 170, 176, 205, 38, 44, 111];

/// The status as a person reads it, coloured for a terminal.
pub struct Text<'a> {
    pub status: &'a Status,
    pub colour: bool,
    /// A holder whose tree has used no CPU for longer than this is said to be idle.
    pub idle_after: i64,
}

/// The escapes, empty when the text is not for a terminal.
struct Palette {
    off: &'static str,
    dim: &'static str,
    bold: &'static str,
    busy: &'static str,
    free: &'static str,
    warn: &'static str,
    queued: &'static str,
    bench: &'static str,
    shared: &'static str,
    agents: bool,
}

impl Palette {
    fn of(colour: bool) -> Palette {
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

    /// One hue per agent, so a session is the same colour wherever it appears.
    fn agent(&self, name: &str) -> String {
        if !self.agents {
            return String::new();
        }
        let hash = name.chars().fold(7u32, |h, c| {
            (h.wrapping_mul(31).wrapping_add(c as u32)) & 0xffff
        });
        format!("\x1b[38;5;{}m", HUES[hash as usize % HUES.len()])
    }
}

/// One job's line and the lines under it, as they are drawn.
struct Row<'a> {
    p: &'a Palette,
    mode: Mode,
    label: &'a str,
    agent: &'a str,
    device: Option<&'a Alias>,
    cmd: &'a str,
}

impl Row<'_> {
    fn hue(&self) -> String {
        self.p.agent(self.agent)
    }

    fn command(&self, out: &mut String) {
        let p = self.p;
        let _ = writeln!(out, "    {}{}{}", p.dim, self.cmd, p.off);
    }

    fn from(&self, out: &mut String) {
        let p = self.p;
        let device = self
            .device
            .map(|d| format!("{} on {}{d}", p.dim, p.off))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "    {}from{} {}{}{}{device}",
            p.dim,
            p.off,
            self.hue(),
            self.agent,
            p.off
        );
    }
}

impl Text<'_> {
    fn holder(&self, out: &mut String, p: &Palette, holder: &Holder) {
        let row = Row {
            p,
            mode: holder.mode,
            label: holder.label.as_str(),
            agent: &holder.agent,
            device: holder.device.as_ref(),
            cmd: &holder.cmd,
        };
        let job = holder
            .job
            .as_ref()
            .map(|job| format!("  {}job {job}{}", p.dim, p.off))
            .unwrap_or_default();
        let lead = format!(
            "  {}{}{}  {}{}{}  {}{}{}  {}pid {}{}{job}",
            p.mode(row.mode),
            row.mode,
            p.off,
            row.hue(),
            row.label,
            p.off,
            p.bold,
            Span(holder.elapsed),
            p.off,
            p.dim,
            holder.pid,
            p.off
        );
        let stop = format!(
            "    {}stop it with: dibs --kill {}{}",
            p.warn, holder.pid, p.off
        );
        if let Some(idle) = holder
            .idle
            .as_ref()
            .filter(|idle| idle.idle_for as i64 > self.idle_after)
        {
            let said = match idle.idle_kind {
                IdleKind::Never => format!("no CPU at all in {}", Span(idle.idle_for)),
                IdleKind::Stalled => format!(
                    "{}s of CPU, none of it in the last {}",
                    holder.cpu,
                    Span(idle.idle_for)
                ),
            };
            let _ = writeln!(
                out,
                "{lead}   {}[IDLE: {said}, it is waiting on something]{}",
                p.warn, p.off
            );
            let _ = writeln!(out, "{stop}");
            row.command(out);
            services(out, p, &holder.services);
            row.from(out);
            if let Some(batch) = &holder.batch {
                batch_lines(out, p, batch, false);
            }
            return;
        }
        let overrun = holder.estimate.as_ref().and_then(Shown::overrun);
        match (&holder.estimate, overrun) {
            (_, Some(overrun)) => {
                let _ = writeln!(out, "{lead}   {}[STUCK? {overrun}]{}", p.warn, p.off);
                let _ = writeln!(out, "{stop}");
            }
            (Some(shown), None) => {
                let _ = writeln!(
                    out,
                    "{lead}   {}[{}]{}",
                    p.dim,
                    shown.said(holder.mode),
                    p.off
                );
            }
            (None, None) => {
                let _ = writeln!(
                    out,
                    "{lead}   {}[no history for this one yet]{}",
                    p.dim, p.off
                );
            }
        }
        row.command(out);
        services(out, p, &holder.services);
        row.from(out);
        if let Some(batch) = &holder.batch {
            batch_lines(out, p, batch, true);
        }
        if let Some(output) = &holder.output {
            let _ = writeln!(
                out,
                "    {}writing {output}{}   {}(dibs --on {} --out {}){}",
                p.dim, p.off, p.dim, self.status.scene.host, holder.pid, p.off
            );
        }
    }

    fn waiter(&self, out: &mut String, p: &Palette, waiter: &Waiter, total: usize) {
        let row = Row {
            p,
            mode: waiter.mode,
            label: waiter.label.as_str(),
            agent: &waiter.agent,
            device: waiter.device.as_ref(),
            cmd: &waiter.cmd,
        };
        let when = match waiter.eta {
            Some(0) => format!("   {}[starts as soon as the lock frees]{}", p.dim, p.off),
            Some(eta) => format!("   {}[~{} until it starts]{}", p.dim, Span(eta), p.off),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "  {}queued {} of {total}{}: {}{}{}  {}{}{}  waiting {}{}{}  {}pid {}{}{when}",
            p.queued,
            waiter.position,
            p.off,
            p.mode(row.mode),
            row.mode,
            p.off,
            row.hue(),
            row.label,
            p.off,
            p.bold,
            Span(waiter.waiting),
            p.off,
            p.dim,
            waiter.pid,
            p.off
        );
        row.command(out);
        row.from(out);
        if let Some(batch) = &waiter.batch {
            batch_lines(out, p, batch, true);
        }
    }

    /// What outlived its job, which holds no lock and so is in no record: it runs beside
    /// whatever is measured next.
    fn leftovers(&self, out: &mut String, p: &Palette) {
        let leftovers = &self.status.scene.leftovers;
        if leftovers.is_empty() {
            return;
        }
        let _ = writeln!(
            out,
            "{}dibs: LEFT RUNNING.{} These outlived the jobs that started them, and hold no lock:",
            p.warn, p.off
        );
        for leftover in leftovers {
            let line = format!("    {}", leftover.described);
            let _ = writeln!(
                out,
                "{}  (job {}, {})",
                line.chars().take(DESCRIBED).collect::<String>(),
                leftover.job,
                leftover.label
            );
        }
        out.push_str("  Stop one with: dibs --kill <pid>\n");
    }

    /// What is said when no record holds the lock.
    fn unheld(&self, out: &mut String, p: &Palette) {
        let status = self.status;
        match status.state {
            LockState::Orphan => {
                let _ = writeln!(
                    out,
                    "{}dibs: LOCKED BY AN ORPHAN.{} No holder record, but the lock is taken, so",
                    p.busy, p.off
                );
                out.push_str("  something outlived its parent. Nothing can run until it goes.\n");
                out.push_str("  holding it:\n");
                for orphan in &status.scene.orphans {
                    let line = format!("    {}", orphan.described);
                    let _ = writeln!(out, "{}", line.chars().take(DESCRIBED).collect::<String>());
                }
                out.push_str("  Stop it with: dibs --kill <pid> --anyone\n");
            }
            LockState::Busy if status.scene.unseen => {
                let _ = writeln!(
                    out,
                    "{}dibs: the lock is taken and nothing here reports holding it.{}",
                    p.busy, p.off
                );
                out.push_str(
                    "  Whatever holds it is out of this account's sight, so it cannot be named.\n",
                );
            }
            LockState::Busy => {
                let _ = writeln!(
                    out,
                    "{}dibs: busy{}, a client has just taken the lock and is recording it.",
                    p.busy, p.off
                );
            }
            _ => {
                let _ = writeln!(out, "{}dibs: idle{}", p.free, p.off);
            }
        }
    }
}

impl fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = self.status;
        let p = Palette::of(self.colour);
        let mut out = String::new();
        if let Some(listing) = &status.scene.listing {
            let _ = writeln!(out, "  [{}]", listing.dir.display());
            for entry in &listing.entries {
                let _ = writeln!(out, "  {entry}");
            }
            let _ = writeln!(
                out,
                "  [{}: {} runs recorded]",
                listing.history.display(),
                listing.runs
            );
        }
        let queued = match status.queue.len() {
            0 => String::new(),
            n => format!("{} ({n} queued){}", p.dim, p.off),
        };
        match status.holders.first().map(|h| h.mode) {
            Some(Mode::Bench) => {
                let _ = writeln!(
                    out,
                    "{}dibs: BUSY, benchmark in progress{}{queued}",
                    p.busy, p.off
                );
            }
            Some(_) => {
                let _ = writeln!(out, "{}dibs: in use, shared{}{queued}", p.warn, p.off);
            }
            None => self.unheld(&mut out, &p),
        }
        for holder in &status.holders {
            self.holder(&mut out, &p, holder);
        }
        let total = status.queue.len();
        for waiter in &status.queue {
            self.waiter(&mut out, &p, waiter, total);
        }
        if total > 1 {
            let _ = writeln!(
                out,
                "  {}(queued in arrival order; the kernel picks the actual wake order){}",
                p.dim, p.off
            );
        }
        self.leftovers(&mut out, &p);
        f.write_str(&out)
    }
}

impl Shown {
    /// What the history says, then what that leaves: `usually 5m00s over 3 runs, ~2m00s left`.
    fn said(&self, mode: Mode) -> String {
        let mut left = match (self.remaining, self.remaining_kind) {
            (Some(left), Some(Remaining::Bound)) => {
                format!("under {} left if it runs true to form", Span(left))
            }
            (Some(left), _) => format!("~{} left", Span(left)),
            (None, _) => "longer than it has ever taken".to_string(),
        };
        let mut usual = if self.wide {
            let low = match self.low {
                0 => "under a second".to_string(),
                low => Span(low).to_string(),
            };
            format!(
                "anywhere from {low} to {} over {} runs",
                Span(self.high),
                self.runs
            )
        } else if self.median == 0 {
            left.clear();
            format!("under a second over {} runs", self.runs)
        } else if self.runs == 1 {
            format!("ran once, in {}", Span(self.median))
        } else {
            format!("usually {} over {} runs", Span(self.median), self.runs)
        };
        usual = match self.scope {
            Scope::Agent => format!("nothing on this one; this agent's other {mode} jobs: {usual}"),
            Scope::Mode => {
                format!("nothing on this one; every {mode} job on the machine: {usual}")
            }
            Scope::This if self.other => format!("nothing with these values; with others: {usual}"),
            Scope::This => usual,
        };
        match left.is_empty() {
            true => usual,
            false => format!("{usual}, {left}"),
        }
    }
}

fn services(out: &mut String, p: &Palette, services: &[Service]) {
    for service in services {
        let _ = writeln!(
            out,
            "    {}with {}, pid {}:{} {}{}{}",
            p.dim, service.name, service.pid, p.off, p.dim, service.command, p.off
        );
    }
}

/// The batch's lines under a job; an idle holder's time left is not estimated.
fn batch_lines(out: &mut String, p: &Palette, batch: &BatchShown, estimated: bool) {
    let _ = writeln!(
        out,
        "    {}batch{} {}{}, step {} of {}: {}{}",
        p.dim, p.off, batch.id, p.dim, batch.k, batch.n, batch.step, p.off
    );
    if batch.here > 0 {
        let _ = writeln!(out, "    {}then here:{} {}", p.dim, p.off, batch.next);
    }
    if batch.elsewhere > 0 {
        let _ = writeln!(
            out,
            "    {}then on other machines:{} {}",
            p.dim, p.off, batch.far
        );
    }
    let said = match batch.left.as_ref().filter(|_| estimated) {
        None => "unknown, this step's own time cannot be estimated".to_string(),
        Some(left) if left.left_partial => format!(
            "over {}, since some of what is ahead has no history",
            Span(left.left)
        ),
        Some(left) => format!("~{}", Span(left.left)),
    };
    let _ = writeln!(out, "    {}batch time left here: {said}{}", p.dim, p.off);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Label, status::base::Scene};

    fn status(state: LockState, scene: Scene) -> Status {
        Status {
            t: 0,
            state,
            cores: 1,
            load: 0,
            caches: Vec::new(),
            clones: Vec::new(),
            holders: Vec::new(),
            queue: Vec::new(),
            scene,
        }
    }

    fn text(status: &Status) -> String {
        Text {
            status,
            colour: false,
            idle_after: 60,
        }
        .to_string()
    }

    #[test]
    fn a_lock_taken_out_of_sight_says_it_cannot_be_named() {
        let unseen = Scene {
            unseen: true,
            ..Scene::default()
        };
        let said = text(&status(LockState::Busy, unseen));
        assert!(said.starts_with("dibs: the lock is taken and nothing here reports holding it.\n"));
        assert_eq!(said.lines().count(), 2);
        let recording = text(&status(LockState::Busy, Scene::default()));
        assert!(recording.starts_with("dibs: busy, a client has just taken the lock"));
    }

    #[test]
    fn a_holder_with_a_job_names_it_beside_its_pid() {
        let mut held = status(LockState::Shared, Scene::default());
        held.holders.push(Holder {
            mode: Mode::Shared,
            pid: 42,
            job: Some(crate::JobId::new("20261001120000-42")),
            label: Label::new("build"),
            agent: "an agent".into(),
            device: None,
            cmd: "cargo build".into(),
            started: 0,
            elapsed: 65,
            cpu: 0,
            estimate: None,
            output: None,
            cpu_rate: None,
            idle: None,
            batch: None,
            services: Vec::new(),
        });
        let said = text(&held);
        assert!(
            said.contains("  shared  build  1m05s  pid 42  job 20261001120000-42   [no history"),
            "{said}"
        );
    }

    #[test]
    fn agents_keep_one_hue_each() {
        let p = Palette::of(true);
        assert_eq!(p.agent("session a"), p.agent("session a"));
        assert!(Palette::of(false).agent("session a").is_empty());
    }
}
