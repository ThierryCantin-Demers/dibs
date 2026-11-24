use crate::{
    job::Tether,
    platform::{Host, Platform as _},
};
use dibs_format::{JobId, LockRecord, Span, status::Ahead};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// How long a call waits for the join guard before it calls its holder stuck.
pub const GUARD_WAIT: Duration = Duration::from_secs(5);

/// A record outlives a pid that comes round again only by being older than the process that has
/// it now; two seconds of slack for the rounding.
const SAME_PROCESS_SLACK: u64 = 2;
/// A batch's cancellation refuses its later steps here for a day.
const CANCELLED_FOR: Duration = Duration::from_secs(Span::DAY.0);
/// What `idle.<pid>` holds for a job that only waits for an idle machine and is not parked once it
/// runs, as a transfer; a parked job's is empty.
const IDLE_WAITS: &str = "waits";

/// The lock directory: the lock files, and the records that say who holds and who waits.
#[derive(Debug, Clone)]
pub struct LockDir {
    pub path: PathBuf,
}

/// A prepare under way, for as long as this lives.
#[must_use]
pub struct Preparing {
    path: PathBuf,
}

impl Preparing {
    /// What the prepare said last, which is what status shows it doing.
    pub fn doing(&self, said: &str) {
        if let Some(line) = said.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
            let _ = fs::write(&self.path, line.strip_prefix("dibs: ").unwrap_or(line));
        }
    }
}

impl Drop for Preparing {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Who holds `rw` this moment, as the kernel says.
#[derive(Debug, Clone, Default)]
pub struct Takers {
    /// Some process can be seen holding it.
    pub seen: bool,
    /// Those that no record accounts for, outside the asking process's own group.
    pub orphans: Vec<u32>,
}

/// A call's urgent job, which has the machine to itself while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Urgent {
    pub pid: u32,
    pub job: JobId,
    pub agent: String,
    pub why: String,
}

/// The two records a job names itself by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Waiting,
    Holder,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Waiting => "waiting",
            Kind::Holder => "holder",
        }
    }
}

impl LockDir {
    pub fn gate(&self) -> PathBuf {
        self.path.join("gate")
    }

    pub fn rw(&self) -> PathBuf {
        self.path.join("rw")
    }

    pub fn ahead(&self) -> PathBuf {
        self.path.join("ahead")
    }

    pub fn file(&self, kind: &str, pid: u32) -> PathBuf {
        self.path.join(format!("{kind}.{pid}"))
    }

    /// When a holder's cap stops it, in seconds since the epoch, for status to say.
    pub fn deadline(&self, pid: u32) -> Option<f64> {
        fs::read_to_string(self.file("deadline", pid))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    pub fn set_deadline(&self, pid: u32, at: Option<f64>) {
        let file = self.file("deadline", pid);
        let _ = match at {
            Some(at) => fs::write(file, format!("{at}\n")),
            None => fs::remove_file(file),
        };
    }

    /// Moves a holder's deadline on by time its cap did not count.
    pub fn defer_deadline(&self, pid: u32, by: Duration) {
        if let Some(at) = self.deadline(pid) {
            self.set_deadline(pid, Some(at + by.as_secs_f64()));
        }
    }

    /// The lock one card is held by, in a namespace of its own: no record is ever named for it.
    pub fn card_lock(&self, pci: &str) -> PathBuf {
        self.path.join("cards").join(pci)
    }

    /// Where a card benchmark hands copies of the machine's lock to the next one to join.
    pub fn cohort_socket(&self, pid: u32) -> PathBuf {
        self.file("cohort", pid)
    }

    /// `card.<pid>`: the cards a card benchmark runs on, which makes it one.
    pub fn mark_cards(&self, pid: u32, pcis: &[String]) {
        let _ = fs::write(self.file("card", pid), format!("{}\n", pcis.join(" ")));
    }

    pub fn cards_of(&self, pid: u32) -> Option<Vec<String>> {
        let text = fs::read_to_string(self.file("card", pid)).ok()?;
        Some(text.split_whitespace().map(str::to_string).collect())
    }

    /// When a benchmark on a card machine arrived, to the nanosecond: two arriving in one second
    /// keep their order, which the record's start in seconds loses.
    pub fn mark_arrived(&self, pid: u32, order: u128) {
        let _ = fs::write(self.file("arrived", pid), format!("{order}\n"));
    }

    /// Before a whole-machine benchmark's record: what a dead one of its pid left.
    pub fn forget_window(&self, pid: u32) {
        for file in self.named(&format!("joined.{pid}")) {
            let _ = fs::remove_file(file);
        }
        let _ = fs::remove_file(self.file("window", pid));
    }

    /// The card benchmarks running as a whole-machine benchmark arrives, which card benchmarks
    /// join ahead of it until one of these, or of the ones that join, ends. Under the join guard
    /// and before its record, so whichever ends after the record is listed.
    pub fn open_window(&self, pid: u32) {
        let holders = self.named("holder");
        let mut members: Vec<String> = self
            .named("admitted")
            .iter()
            .chain(&holders)
            .filter(|file| RecordFile(file).alive())
            .filter_map(|file| RecordFile(file).pid())
            .filter(|&p| p != pid)
            .filter_map(|p| self.member(p).filter(|m| self.member_live(m)))
            .map(|m| m.to_string())
            .collect();
        members.sort_unstable();
        members.dedup();
        let draft = self.file("window-draft", pid);
        if fs::write(&draft, format!("{}\n", members.join(" "))).is_ok() {
            let _ = fs::rename(&draft, self.file("window", pid));
        }
    }

    /// A card benchmark ends when its records go, which a runner killed outright never removes,
    /// so one whose process is gone, a zombie, or another process given its pid, has ended too;
    /// so has one sent back to the queue, which keeps its card record.
    fn member_live(&self, member: &Member) -> bool {
        let card = self.file("card", member.pid);
        let admitted = match member.admission {
            0 => self.file("holder", member.pid).exists(),
            admission => self.admission(member.pid) == Some(admission),
        };
        admitted
            && card.exists()
            && RecordFile(&card).alive()
            && Host::running(member.pid)
            && Host::started_at(member.pid) == Some(member.began)
    }

    /// Held while a whole-machine benchmark publishes its window and record, while a card
    /// benchmark counts itself in, and while join records are pruned, so none reads another half
    /// done. Let go when dropped. Each holds it for a few file writes, so one still held after
    /// `within` is a holder stopped mid-way, which nobody should wait out.
    pub fn join_guard(&self, within: Duration) -> io::Result<fs::File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.path.join("join.guard"))?;
        let until = std::time::Instant::now() + within;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(TryLockError::Error(e)) => return Err(e),
                Err(TryLockError::WouldBlock) if std::time::Instant::now() >= until => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "join.guard is held by a runner that has not let go",
                    ));
                }
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn window_open(&self, machine: u32) -> bool {
        let Ok(text) = fs::read_to_string(self.file("window", machine)) else {
            return false;
        };
        let ran: Vec<Member> = text.split_whitespace().filter_map(Member::parse).collect();
        let joined = self.named(&format!("joined.{machine}"));
        !ran.is_empty()
            && ran
                .into_iter()
                .chain(joined.iter().filter_map(|p| Member::joined(p)))
                .all(|m| self.member_live(&m))
    }

    fn machines_before(&self, pid: u32, order: u128) -> Vec<u32> {
        self.records(Kind::Waiting)
            .iter()
            .filter(|w| {
                w.mode == dibs_format::Mode::Bench
                    && w.pid != pid
                    && self.cards_of(w.pid).is_none()
                    && RecordFile(&self.file("waiting", w.pid)).alive()
                    && self
                        .arrived(w.pid)
                        .is_some_and(|theirs| (theirs, w.pid) < (order, pid))
            })
            .map(|w| w.pid)
            .collect()
    }

    /// Whether a whole-machine benchmark that arrived before `order` waits with every card
    /// benchmark it let ahead still running, so more may join them.
    pub fn open_to_join(&self, pid: u32, order: u128) -> bool {
        self.machines_before(pid, order)
            .into_iter()
            .any(|m| self.window_open(m))
    }

    /// `pid`, which holds its cards, as admitted, and among the ones joining each whole-machine
    /// benchmark queued before it. Under the join guard.
    pub fn count_in(&self, pid: u32, order: u128) -> io::Result<()> {
        let written = self.write_count(pid, order);
        if written.is_err() {
            self.leave_windows(pid);
        }
        written
    }

    fn write_count(&self, pid: u32, order: u128) -> io::Result<()> {
        let began = Host::started_at(pid).ok_or_else(|| io::Error::other("no start time"))?;
        let admission = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(1, |since| since.as_nanos().max(1));
        fs::write(self.file("admitted", pid), format!("{admission}\n"))?;
        for machine in self.machines_before(pid, order) {
            fs::write(
                self.file(&format!("joined.{machine}.{began}.{admission}"), pid),
                "",
            )?;
        }
        Ok(())
    }

    /// This admission of `pid`, which a job sent back to the queue and admitted again does not
    /// share with the one a window counted.
    fn admission(&self, pid: u32) -> Option<u128> {
        fs::read_to_string(self.file("admitted", pid))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// `pid` as a window counts it: by its current admission, or by its holder record when a
    /// runner from before admissions holds the card.
    fn member(&self, pid: u32) -> Option<Member> {
        Some(Member {
            pid,
            began: Host::started_at(pid)?,
            admission: self.admission(pid).unwrap_or(0),
        })
    }

    /// Counts `pid` in, then whether it may still run: a member that ended before it counted
    /// closed the window, and a group is started only with no whole-machine benchmark queued
    /// before it. One that may not is taken out again.
    /// A guard still held after `within`, when the call's deadline cut it short, refuses it for
    /// now, as a busy card would; held for the whole of GUARD_WAIT, its holder is stuck.
    pub fn enter_windows(
        &self,
        pid: u32,
        order: u128,
        pcis: &[String],
        entry: Entry,
        within: Duration,
    ) -> io::Result<bool> {
        let _guard = match self.join_guard(within) {
            Ok(guard) => guard,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && within < GUARD_WAIT => {
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        self.count_in(pid, order)?;
        let may = match entry {
            Entry::Starts => self.machines_before(pid, order).is_empty(),
            Entry::Joins => !self.bench_first(pid, order, Some(pcis)),
            Entry::JoinsAhead => {
                !self.bench_first(pid, order, Some(pcis)) && self.open_to_join(pid, order)
            }
        };
        if !may {
            self.leave_windows(pid);
        }
        Ok(may)
    }

    /// Takes back this process's admission; a finished one of the same pid keeps its join
    /// records, which are what close its windows.
    pub fn leave_windows(&self, pid: u32) {
        let me = self.member(pid).filter(|m| m.admission != 0);
        let _ = fs::remove_file(self.file("admitted", pid));
        for joined in self.named("joined") {
            if me.is_some() && Member::joined(&joined) == me {
                let _ = fs::remove_file(joined);
            }
        }
    }

    fn arrived(&self, pid: u32) -> Option<u128> {
        fs::read_to_string(self.file("arrived", pid))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// A benchmark that arrived before `order` and must go first: one for a card among `pcis`,
    /// which None makes every card, or one for the whole machine, which card benchmarks join the
    /// running ones ahead of only until the first of those ends.
    pub fn bench_first(&self, pid: u32, order: u128, pcis: Option<&[String]>) -> bool {
        self.records(Kind::Waiting).iter().any(|w| {
            let Some(theirs) = self.arrived(w.pid) else {
                return false;
            };
            w.mode == dibs_format::Mode::Bench
                && w.pid != pid
                && (theirs, w.pid) < (order, pid)
                && RecordFile(&self.file("waiting", w.pid)).alive()
                && match (self.cards_of(w.pid), pcis) {
                    (Some(cards), Some(pcis)) => cards.iter().any(|c| pcis.contains(c)),
                    (None, Some(_)) => !self.window_open(w.pid),
                    (_, None) => true,
                }
        })
    }

    /// `prep.<pid>` until the guard goes: a prepare's work is the runner's own, which a holder's
    /// CPU leaves out, so status would otherwise call a tree being copied idle.
    pub fn preparing(&self, pid: u32) -> Preparing {
        let path = self.file("prep", pid);
        let _ = fs::write(&path, "");
        Preparing { path }
    }

    /// What the holder's prepare last said, empty before it says anything; None once its tree
    /// is laid out.
    pub fn prepare_of(&self, pid: u32) -> Option<String> {
        fs::read_to_string(self.file("prep", pid)).ok()
    }

    /// Whether the holder's CPU says nothing of its work: a hold's command runs elsewhere, and
    /// a prepare's runs in the runner.
    pub fn works_unseen(&self, pid: u32) -> bool {
        ["hold", "prep"]
            .iter()
            .any(|kind| self.file(kind, pid).exists())
    }

    /// `ahead.<pid>`: a preemptible job, when it arrived in nanoseconds, and the benchmark that
    /// has frozen it, if one has.
    pub fn mark_ahead(&self, pid: u32, arrived: u128, ahead: &Ahead) {
        let said = match ahead {
            Ahead::Running => format!("{arrived}\n"),
            Ahead::Paused(bench) => format!("{arrived} {bench}\n"),
        };
        let _ = fs::write(self.file("ahead", pid), said);
    }

    pub fn ahead_of(&self, pid: u32) -> Option<Ahead> {
        let said = fs::read_to_string(self.file("ahead", pid)).ok()?;
        Some(
            match said.split_whitespace().nth(1).and_then(JobId::checked) {
                Some(bench) => Ahead::Paused(bench),
                None => Ahead::Running,
            },
        )
    }

    /// Whether every live holder is a preemptible job that arrived after `arrived`, which a
    /// benchmark that arrived then has frozen rather than waits for.
    pub fn only_later_preemptible(&self, arrived: u128) -> bool {
        self.records(Kind::Holder)
            .iter()
            .filter(|h| RecordFile(&self.file("holder", h.pid)).alive())
            .all(|h| {
                fs::read_to_string(self.file("ahead", h.pid))
                    .ok()
                    .and_then(|said| said.split_whitespace().next()?.parse::<u128>().ok())
                    .is_some_and(|at| at > arrived)
            })
    }

    /// `pause.<pid>`: a benchmark that has `rw` asks the jobs that started ahead of it to freeze.
    pub fn ask_to_pause(&self, pid: u32, arrived: u128, job: &JobId) {
        let _ = fs::write(self.file("pause", pid), format!("{arrived} {job}\n"));
    }

    /// The benchmark asking to pause that arrived no later than `arrived`; one that arrived after
    /// waits for the job instead, as it would have had the job waited its turn.
    pub fn pausing_before(&self, arrived: u128) -> Option<JobId> {
        self.named("pause").into_iter().find_map(|file| {
            let said = fs::read_to_string(&file).ok()?;
            let (at, job) = said.trim().split_once(' ')?;
            let alive = RecordFile(&file).alive();
            (alive && at.parse::<u128>().ok()? <= arrived).then(|| JobId::checked(job))?
        })
    }

    /// Written beside and renamed into place: a reader between `fs::write`'s truncate and its
    /// write would read an empty record and drop a live job.
    pub fn write(&self, kind: Kind, record: &LockRecord) {
        let fresh = self
            .path
            .join(format!(".new.{}.{}", kind.prefix(), record.pid));
        if fs::write(&fresh, format!("{record}\n")).is_ok() {
            let _ = fs::rename(&fresh, self.file(kind.prefix(), record.pid));
        }
    }

    /// `urgent-claim`, taken for as long as the returned file lives; None while another urgent
    /// job has it. Only urgent jobs touch it, so a look never makes one refused.
    pub fn claim_urgent(&self) -> io::Result<Option<File>> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.join("urgent-claim"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }

    /// `urgent.<pid>`, which every other job reads as the machine being taken.
    pub fn mark_urgent(&self, urgent: &Urgent) -> io::Result<()> {
        let flat = |s: &str| s.replace(['\t', '\n'], " ");
        fs::write(
            self.file("urgent", urgent.pid),
            format!(
                "{}\t{}\t{}\n",
                urgent.job,
                flat(&urgent.agent),
                flat(&urgent.why)
            ),
        )
    }

    /// The live urgent job of a call other than `pid`'s.
    pub fn urgent_besides(&self, pid: u32) -> Option<Urgent> {
        self.named("urgent").into_iter().find_map(|file| {
            let record = RecordFile(&file);
            let of = record.pid().filter(|&of| of != pid)?;
            record.alive().then_some(())?;
            self.urgent_of(of)
        })
    }

    pub fn urgent_of(&self, pid: u32) -> Option<Urgent> {
        let said = fs::read_to_string(self.file("urgent", pid)).ok()?;
        let mut fields = said.trim_end_matches('\n').splitn(3, '\t');
        Some(Urgent {
            pid,
            job: JobId::checked(fields.next()?)?,
            agent: fields.next()?.to_string(),
            why: fields.next().unwrap_or_default().to_string(),
        })
    }

    /// `parked.<pid>`: the job runs nothing until that urgent job ends, which may start beside it.
    pub fn park(&self, pid: u32, urgent: &JobId) {
        let _ = fs::write(self.file("parked", pid), format!("{urgent}\n"));
    }

    pub fn unpark(&self, pid: u32) {
        let _ = fs::remove_file(self.file("parked", pid));
    }

    pub fn parked_for(&self, pid: u32) -> Option<JobId> {
        JobId::checked(fs::read_to_string(self.file("parked", pid)).ok()?.trim())
    }

    /// The live holders besides `pid` that are not parked for its urgent job `job`, which it
    /// waits for: one parked for an urgent job before it may be about to start.
    pub fn unparked(&self, pid: u32, job: &JobId) -> Vec<LockRecord> {
        self.records(Kind::Holder)
            .into_iter()
            .filter(|h| h.pid != pid && self.parked_for(h.pid).as_ref() != Some(job))
            .filter(|h| RecordFile(&self.file("holder", h.pid)).alive())
            .collect()
    }

    /// Whether a live holder besides `pid` parked for `job` is a benchmark, which holds `rw`
    /// exclusive while it runs nothing.
    pub fn bench_parked_for(&self, pid: u32, job: &JobId) -> bool {
        self.records(Kind::Holder).into_iter().any(|h| {
            h.pid != pid
                && h.mode == dibs_format::Mode::Bench
                && self.parked_for(h.pid).as_ref() == Some(job)
        })
    }

    /// Waiting becomes holding, restamped: a holder's clock starts when it takes the lock.
    pub fn hold(&self, record: &LockRecord) {
        self.write(Kind::Waiting, record);
        let _ = fs::rename(
            self.file(Kind::Waiting.prefix(), record.pid),
            self.file(Kind::Holder.prefix(), record.pid),
        );
    }

    /// Every record of a kind, in the order the shell's glob lists them.
    pub fn records(&self, kind: Kind) -> Vec<LockRecord> {
        self.named(kind.prefix())
            .into_iter()
            .filter_map(|file| fs::read_to_string(&file).ok()?.lines().next()?.parse().ok())
            .collect()
    }

    /// The files whose names start with `<prefix>.`, sorted by name.
    pub fn named(&self, prefix: &str) -> Vec<PathBuf> {
        let lead = format!("{prefix}.");
        let mut files: Vec<PathBuf> = fs::read_dir(&self.path)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&lead))
            .map(|e| e.path())
            .collect();
        files.sort();
        files
    }

    /// `pressing.<pid>`: a job asked to be urgent, from its arrival. It parks nothing: a benchmark
    /// parks the shared jobs only once it holds `urgent.<pid>`, the machine's one urgent claim.
    pub fn mark_pressing(&self, pid: u32, why: &str) {
        let _ = fs::write(self.file("pressing", pid), why.replace('\n', " "));
    }

    /// Why the job is urgent, a benchmark that parks the shared jobs or a shared job that only
    /// goes around the gate.
    pub fn urgent_why(&self, pid: u32) -> Option<String> {
        self.urgent_of(pid)
            .map(|u| u.why)
            .or_else(|| fs::read_to_string(self.file("pressing", pid)).ok())
    }

    /// `quiet.<pid>`: since when a `--low-priority` job waiting here has seen nothing else.
    pub fn mark_quiet(&self, pid: u32, since: Option<u64>) {
        let _ = match since {
            Some(since) => fs::write(self.file("quiet", pid), format!("{since}\n")),
            None => fs::remove_file(self.file("quiet", pid)),
        };
    }

    pub fn quiet_since(&self, pid: u32) -> Option<u64> {
        fs::read_to_string(self.file("quiet", pid))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// `stopped.<pid>`: a `--low-priority` job parked after it ran, for how long, when, and for
    /// which job, which status says until it runs again.
    pub fn mark_stopped(&self, pid: u32, stopped: Option<(u64, u64, &str)>) {
        let _ = match stopped {
            Some((ran, at, by)) => fs::write(
                self.file("stopped", pid),
                format!("{ran}\t{at}\t{}\n", by.replace(['\t', '\n'], " ")),
            ),
            None => fs::remove_file(self.file("stopped", pid)),
        };
    }

    /// The `(ran, at, by)` of `mark_stopped`.
    pub fn stopped(&self, pid: u32) -> Option<(u64, u64, String)> {
        let said = fs::read_to_string(self.file("stopped", pid)).ok()?;
        let mut fields = said.trim_end_matches('\n').splitn(3, '\t');
        Some((
            fields.next()?.parse().ok()?,
            fields.next()?.parse().ok()?,
            fields.next()?.to_string(),
        ))
    }

    /// `idle.<pid>`: a job that runs only while nothing else here does, which another such job
    /// does not count as work.
    pub fn mark_idle(&self, pid: u32, parks: bool) {
        let said = if parks { "" } else { IDLE_WAITS };
        let _ = fs::write(self.file("idle", pid), said);
    }

    pub fn idle_only(&self, pid: u32) -> bool {
        self.file("idle", pid).exists()
    }

    /// A job run only while nothing else here runs that still holds, besides `pid`.
    pub fn idle_holding(&self, pid: u32) -> bool {
        self.records(Kind::Holder).iter().any(|r| {
            r.pid != pid
                && r.mode != dibs_format::Mode::Rsh
                && fs::read_to_string(self.file("idle", r.pid)).is_ok_and(|s| s != IDLE_WAITS)
                && RecordFile(&self.file("holder", r.pid)).alive()
        })
    }

    /// Another job that holds or waits here, which a job run only while none does makes way for.
    /// A transfer is not one: it is short and leaves the cards alone, and parking a job for it
    /// would throw away all the job has run.
    pub fn busy_besides(&self, pid: u32) -> Option<LockRecord> {
        [Kind::Holder, Kind::Waiting].into_iter().find_map(|kind| {
            self.records(kind).into_iter().find(|r| {
                r.pid != pid
                    && r.mode != dibs_format::Mode::Rsh
                    && !self.idle_only(r.pid)
                    && RecordFile(&self.file(kind.prefix(), r.pid)).alive()
            })
        })
    }

    /// The batch's later steps are refused here.
    pub fn cancelled(&self, batch: &str) -> bool {
        self.path.join(format!("cancelled.{batch}")).exists()
    }

    /// Everything this process wrote, gone, as its end leaves the directory.
    pub fn clear(&self, pid: u32) {
        for kind in [
            "waiting",
            "holder",
            "work",
            "cpu",
            "batch",
            "hold",
            "prep",
            "with",
            "ahead",
            "pause",
            "subagent",
            "command",
            "user",
            "deadline",
            "urgent",
            "parked",
            "pressing",
            "quiet",
            "stopped",
            "idle",
            "card",
            "cohort",
            "arrived",
            "window",
            "window-draft",
            "admitted",
        ] {
            let _ = fs::remove_file(self.file(kind, pid));
        }
        // A joiner's own mark stays: its end is what closes the window, until that one clears.
        for joined in self.named(&format!("joined.{pid}")) {
            let _ = fs::remove_file(joined);
        }
        for port in self.named("port") {
            if fs::read_to_string(&port).is_ok_and(|by| by.trim() == pid.to_string()) {
                let _ = fs::remove_file(&port);
            }
        }
    }

    /// Who holds `rw`. A process taking it and letting go at once is a client testing it, and the
    /// asking process's own group is the invocation asking, so neither is an orphan; nor is a
    /// card benchmark's tether, which holds the lock for its job until that is gone and then
    /// ends by itself; one started before tethers were marked is known by its runner's record.
    pub fn takers(&self, asking: u32) -> Takers {
        let mine = Host::group_of(asking);
        let holding: Vec<u32> = Host::lock_holders(&self.rw())
            .into_iter()
            .filter(|&pid| Host::exists(pid))
            .collect();
        let recorded =
            |pid: u32| self.file("holder", pid).exists() || self.file("waiting", pid).exists();
        let processes = Host::processes();
        let parent = |pid: u32| processes.iter().find(|p| p.pid == pid).map(|p| p.parent);
        let orphans = holding
            .iter()
            .copied()
            .filter(|&pid| mine.is_none() || Host::group_of(pid) != mine)
            .filter(|&pid| !recorded(pid) && !parent(pid).is_some_and(recorded))
            .filter(|&pid| Host::variable(pid, Tether::MARK).is_none())
            .collect();
        Takers {
            seen: !holding.is_empty(),
            orphans,
        }
    }

    /// Drops what dead jobs left: a record outlives its process only when that was killed outright.
    pub fn prune(&self) {
        for file in self.named("cancelled") {
            if RecordFile(&file).age().is_some_and(|a| a > CANCELLED_FOR) {
                let _ = fs::remove_file(&file);
            }
        }
        for file in self.named("port") {
            let alive = fs::read_to_string(&file)
                .ok()
                .and_then(|pid| pid.trim().parse().ok())
                .is_some_and(|pid| RecordFile(&file).written_by(pid));
            if !alive {
                let _ = fs::remove_file(&file);
            }
        }
        for file in [self.named("holder"), self.named("waiting")].concat() {
            if !RecordFile(&file).alive() {
                let _ = fs::remove_file(&file);
            }
        }
        // By start time, not by pid alone: a lock directory on disk outlives a reboot, after which
        // its pids name other processes.
        for prefix in [
            "cpu",
            "batch",
            "hold",
            "prep",
            "with",
            "ahead",
            "pause",
            "subagent",
            "command",
            "user",
            "deadline",
            "urgent",
            "parked",
            "pressing",
            "quiet",
            "stopped",
            "idle",
            "card",
            "cohort",
            "arrived",
            "window",
            "window-draft",
            "admitted",
        ] {
            for file in self.named(prefix) {
                if !RecordFile(&file).alive() {
                    let _ = fs::remove_file(&file);
                }
            }
        }
        let joins = self.named("joined");
        let guard = (!joins.is_empty()).then(|| self.join_guard(Duration::ZERO));
        for joined in joins
            .iter()
            .filter(|_| guard.as_ref().is_some_and(|g| g.is_ok()))
        {
            let machine: Option<u32> = joined
                .file_name()
                .and_then(|n| n.to_str()?.split('.').nth(1)?.parse().ok());
            if machine.is_none_or(|m| !self.file("window", m).exists()) {
                let _ = fs::remove_file(joined);
            }
        }
    }
}

/// A card benchmark a window counts, as `<pid>:<start>:<admission>`: a pid alone would name
/// whoever has it next, and a pid and start the same job admitted again after a closure.
#[derive(Debug, PartialEq, Eq)]
struct Member {
    pid: u32,
    began: u64,
    /// 0 for a holder whose runner wrote no admission.
    admission: u128,
}

impl Member {
    fn parse(text: &str) -> Option<Member> {
        let mut parts = text.split(':');
        let member = Member {
            pid: parts.next()?.parse().ok()?,
            began: parts.next()?.parse().ok()?,
            admission: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(member)
    }

    /// From `joined.<machine>.<start>.<admission>.<pid>`.
    fn joined(path: &Path) -> Option<Member> {
        let name = path.file_name()?.to_str()?;
        let mut parts = name.split('.').skip(2);
        let began = parts.next()?.parse().ok()?;
        let admission = parts.next()?.parse().ok()?;
        let pid = parts.next()?.parse().ok()?;
        Some(Member {
            pid,
            began,
            admission,
        })
    }
}

impl std::fmt::Display for Member {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.pid, self.began, self.admission)
    }
}

/// A record's file in the lock directory, named `<kind>.<pid>`.
#[derive(Debug, Clone, Copy)]
pub struct RecordFile<'a>(pub &'a Path);

/// How a card benchmark comes to run while a whole-machine benchmark may be queued.
#[derive(Debug, Clone, Copy)]
pub enum Entry {
    /// Holding the machine alone, so it starts a group.
    Starts,
    /// Joining a group, having taken its turn at the gate.
    Joins,
    /// Joining a group past a whole-machine benchmark that keeps the gate.
    JoinsAhead,
}

impl RecordFile<'_> {
    /// The pid its name ends in.
    pub fn pid(self) -> Option<u32> {
        self.0.extension()?.to_str()?.parse().ok()
    }

    fn age(self) -> Option<Duration> {
        fs::metadata(self.0)
            .ok()?
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
    }

    /// Whether the pid still names the process that wrote it: a pid comes round in days on a
    /// busy machine, and a record left by a job killed outright would name whoever has it by
    /// then.
    pub fn written_by(self, pid: u32) -> bool {
        let Some(began) = Host::started_at(pid) else {
            return false;
        };
        let Some(wrote) = fs::metadata(self.0)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        else {
            return true;
        };
        began <= wrote.as_secs() + SAME_PROCESS_SLACK
    }

    /// Whether the process that wrote it is still there.
    pub fn alive(self) -> bool {
        self.pid().is_some_and(|pid| self.written_by(pid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_closes_when_any_member_is_gone_and_never_opens_empty() {
        let dir = LockDir {
            path: std::env::temp_dir().join(format!("dibs-window-{}", std::process::id())),
        };
        fs::create_dir_all(&dir.path).unwrap();
        let (me, gone, machine) = (std::process::id(), u32::MAX - 1, u32::MAX - 2);
        dir.open_window(machine);
        assert!(!dir.window_open(machine), "nothing ran to join");
        dir.mark_cards(me, &["0000:03:00.0".to_string()]);
        dir.count_in(me, 0).unwrap();
        dir.open_window(machine);
        assert!(dir.window_open(machine));
        let listed = fs::read_to_string(dir.file("window", machine)).unwrap();
        fs::write(dir.file("window", machine), format!("{me}:1:1\n")).unwrap();
        assert!(
            !dir.window_open(machine),
            "another process given a member's pid is not that member"
        );
        fs::write(dir.file("window", machine), &listed).unwrap();
        dir.count_in(me, 0).unwrap();
        assert!(
            !dir.window_open(machine),
            "the same job admitted again is not the admission the window counted"
        );
        let counted = listed.trim().rsplit(':').next().unwrap().to_string();
        fs::write(dir.file("admitted", me), counted).unwrap();
        dir.mark_cards(gone, &["0000:83:00.0".to_string()]);
        fs::write(dir.file("admitted", gone), "").unwrap();
        fs::write(dir.file(&format!("joined.{machine}.1.1"), gone), "").unwrap();
        assert!(
            !dir.window_open(machine),
            "a joiner killed outright has ended"
        );
        dir.clear(gone);
        dir.leave_windows(gone);
        assert!(
            !dir.window_open(machine),
            "a joiner that finished keeps its record when another of its pid steps back"
        );
        fs::remove_file(dir.file(&format!("joined.{machine}.1.1"), gone)).unwrap();
        assert!(dir.window_open(machine));
        dir.clear(me);
        assert!(
            !dir.window_open(machine),
            "a member that finished has ended"
        );
        let _ = fs::remove_dir_all(&dir.path);
    }

    #[test]
    fn a_holder_works_unseen_only_while_it_prepares() {
        let dir = LockDir {
            path: std::env::temp_dir().join(format!("dibs-prep-{}", std::process::id())),
        };
        fs::create_dir_all(&dir.path).unwrap();
        assert!(!dir.works_unseen(7));
        let preparing = dir.preparing(7);
        assert!(dir.works_unseen(7));
        assert!(!dir.works_unseen(8), "only the holder preparing");
        assert_eq!(dir.prepare_of(7).as_deref(), Some(""));
        preparing.doing("dibs: waiting for another prepare of /t\n");
        preparing.doing("\n");
        assert_eq!(
            dir.prepare_of(7).as_deref(),
            Some("waiting for another prepare of /t"),
            "the last line it said, without dibs's prefix"
        );
        drop(preparing);
        assert!(!dir.works_unseen(7));
        assert_eq!(dir.prepare_of(7), None);
        let _ = fs::remove_dir_all(&dir.path);
    }
}
