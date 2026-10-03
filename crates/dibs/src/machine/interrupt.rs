use std::{
    os::unix::process::ExitStatusExt as _,
    process::ExitStatus,
    sync::atomic::{AtomicBool, Ordering},
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
