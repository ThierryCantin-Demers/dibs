//! `dibs-orchestrator`: the service that keeps dibs work alive across machines and outlives the
//! laptop that sent it. `docs/design/orchestrator.md` is the design.

mod api;
mod home;
mod inventory;
mod serve;
mod settings;
mod spawn;
mod split;
mod store;
mod worker;

use dibs_format::Exit;
use home::Home;
use std::{path::Path, process::ExitCode};
use store::Store;

const USAGE: &str = "usage:
  dibs-orchestrator serve <coworker>         the forced command of a coworker's key
  dibs-orchestrator worker <submission>      drives one submission (started by serve and resume)
  dibs-orchestrator resume                   starts a worker for every unfinished submission
  dibs-orchestrator pool <pool> [machine...] sets a pool's machines, or prints them
  dibs-orchestrator pools                    prints every pool
  dibs-orchestrator backup <file>            a consistent copy of the store
  dibs-orchestrator restore <file>           the store from a backup, then resume";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let home = Home::from_env();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    // Held shared by everything that uses the store, so a restore never replaces it under one.
    let _gate = match words.first() {
        Some(&"restore") | None => None,
        Some(_) => home.gate().ok().filter(|g| g.lock_shared().is_ok()),
    };
    let code = match words.as_slice() {
        ["serve", coworker] => serve::serve(&home, coworker),
        ["worker", id] if home::component(id) => worker::run(&home, id),
        ["resume"] => resume(&home),
        ["pool", name, machines @ ..] => pool(&home, name, machines),
        ["pools"] => pools(&home),
        ["backup", to] => backup(&home, Path::new(to)),
        ["restore", from] => restore(&home, Path::new(from)),
        _ => {
            eprintln!("{USAGE}");
            Exit::Refused.status()
        }
    };
    ExitCode::from(code.rem_euclid(256) as u8)
}

fn open(home: &Home) -> Result<Store, i32> {
    Store::open(&home.store()).map_err(|e| {
        eprintln!(
            "dibs-orchestrator: the store at {}: {e}",
            home.store().display()
        );
        Exit::Failed.status()
    })
}

fn resume(home: &Home) -> i32 {
    let store = match open(home) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let unfinished = match store.unfinished() {
        Ok(ids) => ids,
        Err(e) => {
            eprintln!("dibs-orchestrator: {e}");
            return Exit::Failed.status();
        }
    };
    let mut code = 0;
    for id in unfinished {
        if worker::driven(home, &id) {
            println!("{id}: driven already");
            continue;
        }
        match spawn::worker(home, &id) {
            Ok(()) => println!("{id}: resumed"),
            Err(why) => {
                eprintln!("{id}: {why}");
                code = Exit::Failed.status();
            }
        }
    }
    code
}

fn pool(home: &Home, pool: &str, machines: &[&str]) -> i32 {
    let store = match open(home) {
        Ok(store) => store,
        Err(code) => return code,
    };
    if !home::component(pool) || !machines.iter().all(|m| home::component(m)) {
        eprintln!("dibs-orchestrator: a pool and its machines are each one name");
        return Exit::Refused.status();
    }
    if !machines.is_empty() {
        let machines: Vec<String> = machines.iter().map(|m| m.to_string()).collect();
        if let Err(e) = store.set_pool(pool, &machines) {
            eprintln!("dibs-orchestrator: {e}");
            return Exit::Failed.status();
        }
    }
    let pools = store.pools().unwrap_or_default();
    println!(
        "{pool}: {}",
        pools.get(pool).map(|m| m.join(" ")).unwrap_or_default()
    );
    0
}

fn pools(home: &Home) -> i32 {
    let store = match open(home) {
        Ok(store) => store,
        Err(code) => return code,
    };
    for (pool, machines) in store.pools().unwrap_or_default() {
        println!("{pool}: {}", machines.join(" "));
    }
    0
}

fn backup(home: &Home, to: &Path) -> i32 {
    let store = match open(home) {
        Ok(store) => store,
        Err(code) => return code,
    };
    match store.backup(to) {
        Ok(()) => {
            println!("backed up {} to {}", home.store().display(), to.display());
            0
        }
        Err(e) => {
            eprintln!("dibs-orchestrator: the backup failed: {e}");
            Exit::Failed.status()
        }
    }
}

/// The store as the backup has it, under a new epoch, so a worker from before the restore that
/// still lives launches nothing more; then every unfinished submission resumes, asking each
/// machine how the attempts the backup left open went.
fn restore(home: &Home, from: &Path) -> i32 {
    if let Err(e) = Store::inspect(from) {
        eprintln!("dibs-orchestrator: {} is no store: {e}", from.display());
        return Exit::Refused.status();
    }
    let gate = match home.gate().map(|f| (f.try_lock(), f)) {
        Ok((Ok(()), gate)) => gate,
        Ok((Err(_), _)) => {
            eprintln!(
                "dibs-orchestrator: workers or calls still use the store; stop them first (systemctl --user stop 'dibs-orch-*'), then restore"
            );
            return Exit::Refused.status();
        }
        Err(e) => {
            eprintln!("dibs-orchestrator: the store's gate: {e}");
            return Exit::Failed.status();
        }
    };
    let store = home.store();
    // Made whole beside the store before anything is moved, so a failed copy leaves it as it was.
    let restoring = store.with_extension("restoring");
    let epoch = std::fs::copy(from, &restoring)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            Store::open(&restoring)
                .and_then(|s| s.bump_epoch())
                .map_err(|e| e.to_string())
        });
    let epoch = match epoch {
        Ok(epoch) => epoch,
        Err(e) => {
            let _ = std::fs::remove_file(&restoring);
            eprintln!(
                "dibs-orchestrator: {} could not be restored: {e}",
                from.display()
            );
            return Exit::Failed.status();
        }
    };
    if store.exists() {
        let aside = format!("before-restore.{}", dibs_format::Moment::epoch_now());
        for (suffix, kept) in [("db", ""), ("db-wal", "-wal"), ("db-shm", "-shm")] {
            let from = store.with_extension(suffix);
            if from.exists()
                && let Err(e) =
                    std::fs::rename(&from, store.with_extension(format!("{aside}{kept}")))
            {
                eprintln!(
                    "dibs-orchestrator: {} could not be set aside: {e}",
                    from.display()
                );
                return Exit::Failed.status();
            }
        }
        println!(
            "the store before the restore is {}",
            store.with_extension(&aside).display()
        );
    }
    if let Err(e) = std::fs::rename(&restoring, &store) {
        eprintln!("dibs-orchestrator: the restored store could not be put in place: {e}");
        return Exit::Failed.status();
    }
    println!("restored {} as epoch {epoch}", from.display());
    drop(gate);
    resume(home)
}
