//! The dibspatcher's state, in SQLite: pools, submissions, their planned steps, every attempt
//! launched, and the bundles it has attested.

use dibs_format::{
    Moment,
    dibspatcher::{Detail, State, StepState, Submission, Summary},
    wire::Trailer,
};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use std::{collections::BTreeMap, path::Path, time::Duration};

pub struct Store {
    db: Connection,
}

/// How a step ended, as `settle` records it.
pub struct Settled<'a> {
    pub state: State,
    pub exit: i32,
    pub trailer: Option<&'a Trailer>,
    pub note: Option<&'a str>,
}

/// What a worker holds a submission by: the store's epoch, which a restore moves on, and the
/// submission's generation, which each new worker moves on. An attempt is launched only while
/// both are still the worker's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fence {
    pub epoch: i64,
    pub generation: i64,
}

#[derive(Debug)]
pub struct Fenced;

/// A step as it is planned when the submission is accepted.
#[derive(Debug, Clone)]
pub struct Planned {
    pub name: String,
    pub machine: Option<String>,
}

/// A submission's stored step.
#[derive(Debug, Clone)]
pub struct Step {
    pub n: i64,
    pub name: String,
    pub state: State,
    /// What it ended with, once it has.
    pub exit: Option<i32>,
}

/// The last attempt launched at a step.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub id: String,
    pub k: i64,
    /// Launched and never heard back from: its worker went before its call ended.
    pub open: bool,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pools (
    pool TEXT NOT NULL, machine TEXT NOT NULL, PRIMARY KEY (pool, machine));
CREATE TABLE IF NOT EXISTS submissions (
    id TEXT PRIMARY KEY,
    coworker TEXT NOT NULL,
    key TEXT NOT NULL,
    agent TEXT NOT NULL,
    work TEXT NOT NULL,
    resolved TEXT,
    what TEXT NOT NULL,
    state TEXT NOT NULL,
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    exit INTEGER,
    note TEXT,
    generation INTEGER NOT NULL DEFAULT 0,
    worker INTEGER,
    cancelled INTEGER NOT NULL DEFAULT 0,
    UNIQUE (coworker, key));
CREATE TABLE IF NOT EXISTS steps (
    submission TEXT NOT NULL,
    n INTEGER NOT NULL,
    name TEXT NOT NULL,
    machine TEXT,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    trailer TEXT,
    exit INTEGER,
    note TEXT,
    PRIMARY KEY (submission, n));
CREATE TABLE IF NOT EXISTS attempts (
    id TEXT PRIMARY KEY,
    submission TEXT NOT NULL,
    step INTEGER NOT NULL,
    k INTEGER NOT NULL,
    epoch INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    machine TEXT NOT NULL,
    launched INTEGER NOT NULL,
    ended INTEGER,
    record TEXT);
CREATE TABLE IF NOT EXISTS bundles (
    digest TEXT NOT NULL,
    inputs TEXT NOT NULL,
    manifest TEXT NOT NULL,
    producer TEXT NOT NULL,
    created INTEGER NOT NULL,
    PRIMARY KEY (digest, inputs));
";

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Store> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(30))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        db.execute_batch(SCHEMA)?;
        Ok(Store { db })
    }

    /// A write transaction taken before anything is read, so what it reads is still so when it
    /// writes: a deferred one reading first fails, rather than waits, when another writer
    /// committed in between.
    fn immediate(&self) -> rusqlite::Result<Transaction<'_>> {
        Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)
    }

    /// The epoch of the store at `path`, read without creating or changing anything: an error
    /// when it is no store of this dibspatcher's.
    pub fn inspect(path: &Path) -> rusqlite::Result<i64> {
        Store::read_only(path)?.epoch()
    }

    /// A copy of the store at `from` as SQLite reads it, what its log holds included, at `to`.
    pub fn snapshot(from: &Path, to: &Path) -> rusqlite::Result<()> {
        Store::read_only(from)?.backup(to)
    }

    fn read_only(path: &Path) -> rusqlite::Result<Store> {
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.query_row("SELECT count(*) FROM submissions", [], |r| {
            r.get::<_, i64>(0)
        })?;
        Ok(Store { db })
    }

    pub fn epoch(&self) -> rusqlite::Result<i64> {
        Ok(self
            .db
            .query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
    }

    /// Moves the epoch on, so a worker that held a submission before a restore launches nothing.
    pub fn bump_epoch(&self) -> rusqlite::Result<i64> {
        let next = self.epoch()? + 1;
        self.db.execute(
            "INSERT INTO meta (key, value) VALUES ('epoch', ?1)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![next.to_string()],
        )?;
        Ok(next)
    }

    pub fn set_pool(&self, pool: &str, machines: &[String]) -> rusqlite::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        tx.execute("DELETE FROM pools WHERE pool = ?1", params![pool])?;
        for machine in machines {
            tx.execute(
                "INSERT INTO pools (pool, machine) VALUES (?1, ?2)",
                params![pool, machine],
            )?;
        }
        tx.commit()
    }

    pub fn pools(&self) -> rusqlite::Result<BTreeMap<String, Vec<String>>> {
        let mut statement = self
            .db
            .prepare("SELECT pool, machine FROM pools ORDER BY pool, machine")?;
        let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
        let mut pools: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in rows {
            let (pool, machine) = row?;
            pools.entry(pool).or_default().push(machine);
        }
        Ok(pools)
    }

    pub fn find(&self, coworker: &str, key: &str) -> rusqlite::Result<Option<String>> {
        self.db
            .query_row(
                "SELECT id FROM submissions WHERE coworker = ?1 AND key = ?2",
                params![coworker, key],
                |r| r.get(0),
            )
            .optional()
    }

    /// Stores a submission and its plan, or finds the one its key stored before. `resolved` is
    /// what the plan was made from, kept so a later change of recipe changes nothing stored.
    pub fn submit(
        &self,
        coworker: &str,
        submission: &Submission,
        what: &str,
        plan: &[Planned],
        resolved: Option<&str>,
    ) -> rusqlite::Result<(String, bool)> {
        let tx = self.immediate()?;
        let seen: Option<String> = tx
            .query_row(
                "SELECT id FROM submissions WHERE coworker = ?1 AND key = ?2",
                params![coworker, submission.key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = seen {
            return Ok((id, true));
        }
        let now = Moment::epoch_now() as i64;
        let id = new_id();
        let work = serde_json::to_string(submission).unwrap_or_default();
        tx.execute(
            "INSERT INTO submissions (id, coworker, key, agent, work, resolved, what, state, created, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'accepted', ?8, ?8)",
            params![
                id,
                coworker,
                submission.key,
                submission.agent,
                work,
                resolved,
                what,
                now
            ],
        )?;
        for (n, step) in plan.iter().enumerate() {
            tx.execute(
                "INSERT INTO steps (submission, n, name, machine, state) VALUES (?1, ?2, ?3, ?4, 'accepted')",
                params![id, n as i64, step.name, step.machine],
            )?;
        }
        tx.commit()?;
        Ok((id, false))
    }

    pub fn submission(&self, id: &str) -> rusqlite::Result<Option<(String, Submission)>> {
        self.db
            .query_row(
                "SELECT coworker, work FROM submissions WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .map(|row| {
                row.and_then(|(coworker, work)| {
                    serde_json::from_str(&work).ok().map(|w| (coworker, w))
                })
            })
    }

    pub fn resolved(&self, id: &str) -> rusqlite::Result<Option<String>> {
        self.db
            .query_row(
                "SELECT resolved FROM submissions WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
    }

    pub fn list(&self, coworker: Option<&str>) -> rusqlite::Result<Vec<Summary>> {
        let mut statement = self.db.prepare(
            "SELECT id, coworker, agent, what, state, created, exit FROM submissions
             WHERE ?1 IS NULL OR coworker = ?1 ORDER BY created DESC, id DESC LIMIT 200",
        )?;
        let rows = statement.query_map(params![coworker], summary)?;
        rows.collect()
    }

    pub fn summary(&self, id: &str) -> rusqlite::Result<Option<Summary>> {
        self.db
            .query_row(
                "SELECT id, coworker, agent, what, state, created, exit FROM submissions WHERE id = ?1",
                params![id],
                summary,
            )
            .optional()
    }

    pub fn detail(&self, id: &str) -> rusqlite::Result<Option<Detail>> {
        let Some(summary) = self.summary(id)? else {
            return Ok(None);
        };
        let note: Option<String> = self.db.query_row(
            "SELECT note FROM submissions WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        let mut statement = self.db.prepare(
            "SELECT name, machine, state, attempts, trailer, note FROM steps
             WHERE submission = ?1 ORDER BY n",
        )?;
        let steps = statement
            .query_map(params![id], |r| {
                Ok(StepState {
                    name: r.get(0)?,
                    machine: r.get(1)?,
                    state: state(&r.get::<_, String>(2)?),
                    attempts: r.get::<_, i64>(3)? as u32,
                    trailer: r
                        .get::<_, Option<String>>(4)?
                        .and_then(|t| serde_json::from_str::<Trailer>(&t).ok()),
                    note: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(Detail {
            summary,
            steps,
            note,
        }))
    }

    pub fn steps(&self, id: &str) -> rusqlite::Result<Vec<Step>> {
        let mut statement = self
            .db
            .prepare("SELECT n, name, state, exit FROM steps WHERE submission = ?1 ORDER BY n")?;
        statement
            .query_map(params![id], |r| {
                Ok(Step {
                    n: r.get(0)?,
                    name: r.get(1)?,
                    state: state(&r.get::<_, String>(2)?),
                    exit: r.get(3)?,
                })
            })?
            .collect()
    }

    /// Takes the submission for this worker: a new generation, which fences any worker before.
    pub fn claim(&self, id: &str, pid: u32) -> rusqlite::Result<Fence> {
        let tx = self.immediate()?;
        let epoch = epoch_in(&tx)?;
        tx.execute(
            "UPDATE submissions SET generation = generation + 1, worker = ?2, updated = ?3,
             state = CASE state WHEN 'accepted' THEN 'running' ELSE state END WHERE id = ?1",
            params![id, pid, Moment::epoch_now() as i64],
        )?;
        let generation: i64 = tx.query_row(
            "SELECT generation FROM submissions WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(Fence { epoch, generation })
    }

    /// Records an attempt as launched, only while `fence` still holds the submission.
    pub fn launch(
        &self,
        fence: Fence,
        id: &str,
        step: i64,
        attempt: &Attempt,
        machine: &str,
    ) -> rusqlite::Result<Result<(), Fenced>> {
        let tx = self.immediate()?;
        let generation: i64 = tx.query_row(
            "SELECT generation FROM submissions WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        if epoch_in(&tx)? != fence.epoch || generation != fence.generation {
            return Ok(Err(Fenced));
        }
        tx.execute(
            "INSERT INTO attempts (id, submission, step, k, epoch, generation, machine, launched)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (id) DO UPDATE SET generation = excluded.generation, epoch = excluded.epoch",
            params![
                attempt.id,
                id,
                step,
                attempt.k,
                fence.epoch,
                fence.generation,
                machine,
                Moment::epoch_now() as i64
            ],
        )?;
        tx.execute(
            "UPDATE steps SET attempts = MAX(attempts, ?3), state = 'running', machine = ?4
             WHERE submission = ?1 AND n = ?2",
            params![id, step, attempt.k, machine],
        )?;
        tx.commit()?;
        Ok(Ok(()))
    }

    pub fn attempt_ended(&self, attempt: &str, record: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE attempts SET ended = ?2, record = ?3 WHERE id = ?1",
            params![attempt, Moment::epoch_now() as i64, record],
        )?;
        Ok(())
    }

    pub fn last_attempt(&self, id: &str, step: i64) -> rusqlite::Result<Option<Attempt>> {
        self.db
            .query_row(
                "SELECT id, k, ended IS NULL FROM attempts WHERE submission = ?1 AND step = ?2
                 ORDER BY k DESC LIMIT 1",
                params![id, step],
                |r| {
                    Ok(Attempt {
                        id: r.get(0)?,
                        k: r.get(1)?,
                        open: r.get(2)?,
                    })
                },
            )
            .optional()
    }

    /// The step's outcome, and the attempt it came from closed with what was heard of it, as
    /// one: an attempt is never closed while its step still waits on it.
    pub fn settle(
        &self,
        id: &str,
        step: i64,
        attempt: Option<(&str, &str)>,
        settled: Settled,
    ) -> rusqlite::Result<()> {
        let Settled {
            state,
            exit,
            trailer,
            note,
        } = settled;
        let trailer = trailer.and_then(|t| serde_json::to_string(t).ok());
        let tx = self.immediate()?;
        if let Some((attempt, heard)) = attempt {
            tx.execute(
                "UPDATE attempts SET ended = ?2, record = ?3 WHERE id = ?1",
                params![attempt, Moment::epoch_now() as i64, heard],
            )?;
        }
        tx.execute(
            "UPDATE steps SET state = ?3, trailer = COALESCE(?4, trailer), note = COALESCE(?5, note),
             exit = ?6 WHERE submission = ?1 AND n = ?2",
            params![id, step, state.as_str(), trailer, note, exit],
        )?;
        tx.commit()
    }

    pub fn finish(
        &self,
        id: &str,
        state: State,
        exit: Option<i32>,
        note: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE submissions SET state = ?2, exit = ?3, note = COALESCE(?4, note), updated = ?5,
             worker = NULL WHERE id = ?1",
            params![id, state.as_str(), exit, note, Moment::epoch_now() as i64],
        )?;
        Ok(())
    }

    /// Marks it cancelled, and says which worker to stop when one drives it.
    pub fn cancel(&self, id: &str) -> rusqlite::Result<Option<u32>> {
        self.db.execute(
            "UPDATE submissions SET cancelled = 1 WHERE id = ?1",
            params![id],
        )?;
        self.db
            .query_row(
                "SELECT worker FROM submissions WHERE id = ?1",
                params![id],
                |r| r.get::<_, Option<u32>>(0),
            )
            .optional()
            .map(Option::flatten)
    }

    pub fn cancelled(&self, id: &str) -> rusqlite::Result<bool> {
        self.db.query_row(
            "SELECT cancelled FROM submissions WHERE id = ?1",
            params![id],
            |r| r.get::<_, bool>(0),
        )
    }

    pub fn unfinished(&self) -> rusqlite::Result<Vec<String>> {
        let mut statement = self.db.prepare(
            "SELECT id FROM submissions WHERE state IN ('accepted', 'running') ORDER BY created",
        )?;
        statement.query_map([], |r| r.get(0))?.collect()
    }

    pub fn attest(
        &self,
        digest: &str,
        inputs: &str,
        manifest: &str,
        producer: &str,
    ) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO bundles (digest, inputs, manifest, producer, created)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                digest,
                inputs,
                manifest,
                producer,
                Moment::epoch_now() as i64
            ],
        )?;
        Ok(())
    }

    pub fn attested(&self, digest: &str, inputs: &str) -> rusqlite::Result<bool> {
        self.db.query_row(
            "SELECT COUNT(*) > 0 FROM bundles WHERE digest = ?1 AND inputs = ?2",
            params![digest, inputs],
            |r| r.get(0),
        )
    }

    /// A consistent copy of the whole store, taken while it is in use.
    pub fn backup(&self, to: &Path) -> rusqlite::Result<()> {
        let mut copy = Connection::open(to)?;
        let backup = rusqlite::backup::Backup::new(&self.db, &mut copy)?;
        backup.run_to_completion(256, Duration::from_millis(0), None)
    }
}

fn epoch_in(tx: &rusqlite::Transaction) -> rusqlite::Result<i64> {
    Ok(tx
        .query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

fn state(text: &str) -> State {
    text.parse().unwrap_or(State::Unknown)
}

fn summary(r: &rusqlite::Row) -> rusqlite::Result<Summary> {
    Ok(Summary {
        id: r.get(0)?,
        coworker: r.get(1)?,
        agent: r.get(2)?,
        what: r.get(3)?,
        state: state(&r.get::<_, String>(4)?),
        created: r.get::<_, i64>(5)? as u64,
        exit: r.get(6)?,
    })
}

/// A submission's id: when it was accepted, and a little randomness so two in one second differ.
fn new_id() -> String {
    let mut bytes = [0u8; 3];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        use std::io::Read as _;
        let _ = random.read_exact(&mut bytes);
    }
    format!(
        "o{}-{}",
        Moment::now().dashed(),
        dibs_format::Hex::hex(&bytes[..])
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dibs_format::dibspatcher::{NamedCall, Work};

    fn store(name: &str) -> (Store, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("dibspatcher-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open(&dir.join("store.db")).unwrap(), dir)
    }

    fn submission(key: &str) -> Submission {
        Submission {
            key: key.into(),
            agent: "agent".into(),
            work: Work::Call(NamedCall {
                on: "box".into(),
                bench: false,
                device: None,
                label: "l".into(),
                command: "true".into(),
                max: None,
            }),
            upload: None,
        }
    }

    #[test]
    fn a_key_sent_twice_stores_one_submission() {
        let (store, dir) = store("twice");
        let plan = [Planned {
            name: "call".into(),
            machine: Some("box".into()),
        }];
        let (id, duplicate) = store
            .submit("alice", &submission("k1"), "w", &plan, None)
            .unwrap();
        assert!(!duplicate);
        assert_eq!(
            store
                .submit("alice", &submission("k1"), "w", &plan, None)
                .unwrap(),
            (id.clone(), true)
        );
        let (other, duplicate) = store
            .submit("bob", &submission("k1"), "w", &plan, None)
            .unwrap();
        assert!(!duplicate && other != id, "a key is the coworker's own");
        assert_eq!(store.list(Some("alice")).unwrap().len(), 1);
        assert_eq!(store.list(None).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_worker_claimed_over_or_restored_over_launches_nothing() {
        let (store, dir) = store("fence");
        let plan = [Planned {
            name: "call".into(),
            machine: Some("box".into()),
        }];
        let (id, _) = store
            .submit("alice", &submission("k"), "w", &plan, None)
            .unwrap();
        let first = store.claim(&id, 1).unwrap();
        let attempt = |k| Attempt {
            id: format!("{id}.0.{k}"),
            k,
            open: true,
        };
        assert!(
            store
                .launch(first, &id, 0, &attempt(1), "box")
                .unwrap()
                .is_ok()
        );
        let second = store.claim(&id, 2).unwrap();
        assert!(
            store
                .launch(first, &id, 0, &attempt(2), "box")
                .unwrap()
                .is_err()
        );
        assert!(
            store
                .launch(second, &id, 0, &attempt(2), "box")
                .unwrap()
                .is_ok()
        );
        assert!(store.last_attempt(&id, 0).unwrap().unwrap().open);
        store.attempt_ended(&attempt(2).id, "{}").unwrap();
        assert!(!store.last_attempt(&id, 0).unwrap().unwrap().open);
        store.bump_epoch().unwrap();
        assert!(
            store
                .launch(second, &id, 0, &attempt(3), "box")
                .unwrap()
                .is_err()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
