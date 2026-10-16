//! What a tool saved in a tree's copies, kept here as well, per machine series: sent to the
//! machine before a run and fetched back after it, so a new tree on any machine of the series, a
//! rental after this one included, starts from what was already saved.

use crate::{
    call::{MachineCall, Origin, Output, Sync},
    caller::Caller,
    cli::Call,
    machine::{Route, Session, Stream},
    paths::Paths,
};
use dibs_format::{FileName, MachineName};
use std::{collections::BTreeMap, path::PathBuf};

/// The copied directories of one machine, and this computer's copy of them for its series.
pub struct KeptStores {
    machine: Option<MachineName>,
    /// Each directory, as a path under the machine's home.
    dirs: Vec<String>,
    /// Laid out as under the machine's home.
    here: PathBuf,
}

/// rsync's exit when something asked for was not there, as a store a tool never wrote is not.
const NOTHING_THERE: i32 = 23;

impl KeptStores {
    /// The directories `copies` names under the machine's home. None when there are none, and on
    /// the machine itself, where they are this computer's own.
    pub fn of(machine: Option<MachineName>, copies: &BTreeMap<String, String>) -> Option<Self> {
        let dirs: Vec<String> = copies
            .values()
            .filter_map(|dir| dir.strip_prefix("~/"))
            .map(|dir| dir.trim_end_matches('/').to_string())
            .collect();
        if dirs.is_empty() {
            return None;
        }
        let call = Call {
            on: machine.clone(),
            ..Call::default()
        };
        let caller = Caller::from_env();
        let reached = MachineCall::new(&call, &caller).ok()?;
        let target = reached.target().ok()?;
        if Session::new(&target, &reached.here).route == Route::Here {
            return None;
        }
        let here = Paths::from_env()
            .kept()?
            .join(FileName::of(&target.series_key()).as_str());
        Some(KeptStores {
            machine,
            dirs,
            here,
        })
    }

    /// This computer's copy into the machine's directories, each file only where it is newer.
    pub fn send(&self) {
        for dir in &self.dirs {
            let ours = self.here.join(dir);
            if ours.is_dir() {
                self.transfer(&format!("{}/", ours.display()), &format!(":~/{dir}/"), dir);
            }
        }
    }

    /// The machine's directories into this computer's copy, the same way.
    pub fn fetch(&self) {
        for dir in &self.dirs {
            let ours = self.here.join(dir);
            if std::fs::create_dir_all(&ours).is_ok() {
                self.transfer(&format!(":~/{dir}/"), &format!("{}/", ours.display()), dir);
            }
        }
    }

    /// Quiet unless it failed, and never the run's failure: a store not kept costs a tune.
    fn transfer(&self, from: &str, to: &str, dir: &str) {
        let args: Vec<String> = ["-rlt", "--update", from, to]
            .iter()
            .map(|a| a.to_string())
            .collect();
        let call = Call {
            on: self.machine.clone(),
            ..Call::default()
        };
        let caller = Caller::from_env();
        let mut said = Vec::new();
        let mut keep = |_: Stream, line: &[u8]| said.extend_from_slice(line);
        let code = MachineCall::new(&call, &caller).and_then(|machine| {
            Sync {
                machine: &machine,
                args: &args,
                origin: Origin::Words,
            }
            .answer_into(&mut Output::Lines(&mut keep))
        });
        match code {
            Ok(0 | NOTHING_THERE) => {}
            Ok(code) => {
                eprint!("{}", String::from_utf8_lossy(&said));
                eprintln!(
                    "dibs: ~/{dir} was not kept, so a new tree of this machine's series may tune again (rsync exited {code})"
                );
            }
            Err(e) => eprint!("{e}"),
        }
    }
}
