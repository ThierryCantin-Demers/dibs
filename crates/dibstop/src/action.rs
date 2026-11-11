//! One-off `dibs` calls a key starts, whose output comes back as an overlay.

use std::{sync::mpsc::Sender, thread};

pub use dibstop_view::action::Action;

use crate::feed::Msg;

/// Runs an action on a thread of its own, and sends what it printed back.
pub trait Start {
    fn start(self, tx: Sender<Msg>);
}

impl Start for Action {
    fn start(self, tx: Sender<Msg>) {
        thread::spawn(move || {
            let body = self.run();
            let _ = tx.send(Msg::Action {
                title: self.title,
                body,
            });
        });
    }
}
