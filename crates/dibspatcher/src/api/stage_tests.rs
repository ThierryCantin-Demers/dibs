//! A refused seal clears its stage, so each test lays out a store and a stage of its own.

use super::cleared;
use crate::store::{Planned, Store};
use dibs_format::dibspatcher::{NamedCall, Submission, Work};
use std::{fs, path::PathBuf};

/// A store and a stage `stage/k` for one test, removed when dropped.
struct Sent {
    dir: PathBuf,
    store: Store,
}

impl Sent {
    fn new(name: &str) -> Sent {
        let dir =
            std::env::temp_dir().join(format!("dibspatcher-sent-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("stage/k")).unwrap();
        fs::write(dir.join("stage/k/a"), "left by an earlier send").unwrap();
        let store = Store::open(&dir.join("store.db")).unwrap();
        Sent { dir, store }
    }

    fn stage(&self) -> PathBuf {
        self.dir.join("stage/k")
    }

    fn accept(&self) {
        let submission = Submission {
            key: "k".into(),
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
        };
        let plan = [Planned {
            name: "call".into(),
            machine: None,
        }];
        self.store
            .submit("alice", &submission, "w", &plan, None)
            .unwrap();
    }
}

impl Drop for Sent {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_refused_stage_no_submission_holds_is_cleared() {
    let sent = Sent::new("cleared");
    let said = cleared(&sent.store, "alice", "k", &sent.stage());
    assert_eq!(
        (said.as_str(), sent.stage().exists()),
        ("its stage is cleared, so send it again", false)
    );
}

#[test]
fn a_refused_stage_a_submission_was_accepted_from_is_kept() {
    let sent = Sent::new("kept");
    sent.accept();
    cleared(&sent.store, "alice", "k", &sent.stage());
    assert!(sent.stage().join("a").exists());
}
