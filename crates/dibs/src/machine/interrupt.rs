use std::{
    os::unix::process::ExitStatusExt as _,
    process::ExitStatus,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

/// Ctrl-C while a child runs belongs to the child: this process carries on, and dies of it only
/// if the child did, as a shell does.
pub struct Interrupt {
    previous: libc::sighandler_t,
}

static HEARD: AtomicBool = AtomicBool::new(false);

extern "C" fn noted(_: libc::c_int) {
    HEARD.store(true, Ordering::Relaxed);
}

impl Interrupt {
    /// A Ctrl-C this process was started ignoring stays ignored.
    pub fn defer() -> Interrupt {
        let handler = noted as extern "C" fn(libc::c_int);
        // SAFETY: the handler does nothing, and is replaced again on drop.
        let previous = unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) };
        if previous == libc::SIG_IGN {
            // SAFETY: puts back the disposition just replaced.
            unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
        }
        Interrupt { previous }
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
        // SAFETY: puts back the handler `defer` replaced.
        unsafe { libc::signal(libc::SIGINT, self.previous) };
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

extern "C" fn relay(signal: libc::c_int) {
    RELAYED_SIGNAL.store(signal, Ordering::Relaxed);
    let child = RELAY_TO.load(Ordering::Relaxed);
    if child > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(child, signal) };
    }
}

impl Relayed {
    pub fn to(child: u32) -> Relayed {
        RELAY_TO.store(child as i32, Ordering::Relaxed);
        let handler = relay as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: the handler makes async-signal-safe calls only, and drop restores these.
        let previous = RELAYED.map(|signal| unsafe { libc::signal(signal, handler) });
        Relayed { previous }
    }

    /// The child has ended: a signal it was relayed now ends this process too.
    pub fn pass_on(self) {
        drop(self);
        let signal = RELAYED_SIGNAL.swap(0, Ordering::Relaxed);
        if signal != 0 {
            // SAFETY: restores the default action and raises the signal on this process.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
        }
    }
}

impl Drop for Relayed {
    fn drop(&mut self) {
        RELAY_TO.store(0, Ordering::Relaxed);
        for (signal, previous) in RELAYED.iter().zip(self.previous) {
            // SAFETY: puts back the handler `to` replaced.
            unsafe { libc::signal(*signal, previous) };
        }
    }
}
