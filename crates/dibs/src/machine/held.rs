use crate::machine::session::Message;
use dibs_format::wire::Picked;
use std::{
    io,
    sync::mpsc::{Receiver, Sender},
    thread::JoinHandle,
};

/// The machine's side of a hold, started: what it says is passed on as it comes.
pub struct Holder {
    /// Says once, when the lock is held; closes when the machine's side ends.
    pub held: Receiver<Held>,
    /// The machine side's exit.
    pub ended: JoinHandle<io::Result<i32>>,
}

/// The lock is held for the command here.
pub struct Held {
    pub ports: Vec<Picked>,
    pub release: Release,
}

/// Ends a hold with its command's status. Dropped unsent, the machine learns its caller is gone.
pub struct Release(pub Sender<Message>);

impl Release {
    pub fn send(self, status: i32) {
        let _ = self.0.send(Message::Release(status));
    }
}
