use std::{
    os::unix::process::ExitStatusExt as _,
    process::ExitStatus,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};

/// Ctrl-C while a child runs belongs to the child: this process carries on, and dies of it only
/// if the child did, as a shell does.
pub struct Interrupt(());

/// The calls deferring Ctrl-C at once, which threads of one viewer make, and the handler the
/// first of them replaced, put back by the last to end.
struct Deferrals {
    calls: usize,
    previous: libc::sighandler_t,
}

static HEARD: AtomicBool = AtomicBool::new(false);
static DEFERRALS: Mutex<Deferrals> = Mutex::new(Deferrals {
    calls: 0,
    previous: libc::SIG_DFL,
});

extern "C" fn noted(_: libc::c_int) {
    HEARD.store(true, Ordering::Relaxed);
}

impl Interrupt {
    /// A Ctrl-C this process was started ignoring stays ignored.
    pub fn defer() -> Interrupt {
        let mut deferrals = DEFERRALS.lock().unwrap_or_else(PoisonError::into_inner);
        if deferrals.calls == 0 {
            let handler = noted as extern "C" fn(libc::c_int);
            // SAFETY: the handler does nothing, and is replaced again by the last drop.
            let previous = unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) };
            if previous == libc::SIG_IGN {
                // SAFETY: puts back the disposition just replaced.
                unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
            }
            deferrals.previous = previous;
        }
        deferrals.calls += 1;
        Interrupt(())
    }

    /// Whether Ctrl-C arrived while a child had it, whatever the child did with it.
    pub fn heard() -> bool {
        HEARD.load(Ordering::Relaxed)
    }

    /// Ends this process as Ctrl-C would have.
    pub fn raise() {
        // SAFETY: restores the default action and raises it on this process.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::raise(libc::SIGINT);
        }
    }

    /// A child that died of Ctrl-C takes this process with it.
    pub fn pass_on(status: ExitStatus) {
        if status.signal() == Some(libc::SIGINT) {
            Interrupt::raise();
        }
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        let mut deferrals = DEFERRALS.lock().unwrap_or_else(PoisonError::into_inner);
        deferrals.calls -= 1;
        if deferrals.calls == 0 {
            // SAFETY: puts back the handler the first `defer` replaced.
            unsafe { libc::signal(libc::SIGINT, deferrals.previous) };
        }
    }
}

/// TERM and HUP while a child runs reach the child too, as a parent-death signal would on Linux
/// alone, so a sync told to stop does not leave rsync transferring under the lock; this process
/// then dies of the signal once the child has ended.
pub struct Relayed {
    previous: [libc::sighandler_t; 2],
}

const RELAYED: [libc::c_int; 2] = [libc::SIGTERM, libc::SIGHUP];
static RELAY_TO: AtomicI32 = AtomicI32::new(0);
static RELAYED_SIGNAL: AtomicI32 = AtomicI32::new(0);
/// A signal heard and not yet sent on: whichever of the handler and `to` takes it sends it, so it
/// reaches a child that was starting as it arrived, and reaches it once.
static UNSENT: AtomicI32 = AtomicI32::new(0);

extern "C" fn relay(signal: libc::c_int) {
    RELAYED_SIGNAL.store(signal, Ordering::SeqCst);
    UNSENT.store(signal, Ordering::SeqCst);
    let child = RELAY_TO.load(Ordering::SeqCst);
    if child > 0 {
        Relayed::send(child, UNSENT.swap(0, Ordering::SeqCst));
    }
}

impl Relayed {
    /// TERM and HUP are caught from before the child starts, which `to` then names.
    pub fn catch() -> Relayed {
        RELAY_TO.store(0, Ordering::SeqCst);
        let handler = relay as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: the handler makes async-signal-safe calls only, and drop restores these.
        let previous = RELAYED.map(|signal| unsafe { libc::signal(signal, handler) });
        Relayed { previous }
    }

    /// The child started: what was heard while it did is sent on now, and what comes later as
    /// it arrives.
    pub fn to(&self, child: u32) {
        RELAY_TO.store(child as i32, Ordering::SeqCst);
        Relayed::send(child as i32, UNSENT.swap(0, Ordering::SeqCst));
    }

    /// The child has ended: a signal it was relayed now ends this process too.
    pub fn pass_on(self) {
        drop(self);
        let signal = RELAYED_SIGNAL.swap(0, Ordering::SeqCst);
        if signal != 0 {
            // SAFETY: restores the default action and raises the signal on this process.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
        }
    }

    fn send(child: i32, signal: libc::c_int) {
        if signal != 0 {
            // SAFETY: kill is async-signal-safe.
            unsafe { libc::kill(child, signal) };
        }
    }
}

impl Drop for Relayed {
    fn drop(&mut self) {
        RELAY_TO.store(0, Ordering::SeqCst);
        for (signal, previous) in RELAYED.iter().zip(self.previous) {
            // SAFETY: puts back the handler `to` replaced.
            unsafe { libc::signal(*signal, previous) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disposition() -> libc::sighandler_t {
        // SAFETY: a null action only reads the current one into `current`.
        unsafe {
            let mut current: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGINT, std::ptr::null(), &mut current);
            current.sa_sigaction
        }
    }

    #[test]
    fn calls_that_end_out_of_order_put_back_the_handler_the_first_found() {
        let before = disposition();
        let deferred = match before {
            libc::SIG_IGN => libc::SIG_IGN,
            _ => noted as extern "C" fn(libc::c_int) as libc::sighandler_t,
        };
        let first = Interrupt::defer();
        let second = Interrupt::defer();
        drop(first);
        assert_eq!(disposition(), deferred, "while the second still runs");
        drop(second);
        assert_eq!(disposition(), before);
    }
}
