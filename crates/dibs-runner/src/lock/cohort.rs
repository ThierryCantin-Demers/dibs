use crate::lock::{LockDir, RecordFile};
use std::{
    fs::{self, File},
    io, mem,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
        unix::{
            fs::PermissionsExt as _,
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// How often a member looks for the next one to join, and how long a joiner waits for its copies.
const POLL: Duration = Duration::from_millis(20);
const ANSWER: Duration = Duration::from_secs(2);

/// A card benchmark handing copies of `rw` and `ahead` to the next card benchmark to join. A
/// `flock` belongs to the open file, not to a process, so the machine stays locked for as long
/// as any member still has a copy, and goes by the kernel's hand when the last one closes.
pub struct Sharing {
    stop: Arc<AtomicBool>,
    serving: Option<JoinHandle<()>>,
    path: PathBuf,
}

impl Sharing {
    pub fn start(dir: &LockDir, pid: u32, rw: File, ahead: File) -> io::Result<Sharing> {
        let path = dir.cohort_socket(pid);
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o660));
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let serving = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = send(&stream, &[rw.as_raw_fd(), ahead.as_raw_fd()]);
                    }
                    Err(_) => thread::sleep(POLL),
                }
            }
        });
        Ok(Sharing {
            stop,
            serving: Some(serving),
            path,
        })
    }
}

/// Stops handing out copies and closes its own, before the member's lock goes.
impl Drop for Sharing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

/// Copies of `rw` and `ahead` from any live member, as `(rw, ahead)`; None when no member
/// answers by `deadline`, or in ANSWER each, which leaves the joiner to take the machine itself.
pub fn join(dir: &LockDir, me: u32, deadline: Option<Instant>) -> Option<(File, File)> {
    dir.named("cohort").into_iter().find_map(|path| {
        let pid = RecordFile(&path).pid()?;
        if pid == me || !RecordFile(&path).alive() {
            return None;
        }
        let left = deadline.map_or(ANSWER, |d| d.saturating_duration_since(Instant::now()));
        let wait = left.min(ANSWER).max(Duration::from_millis(50));
        let stream = UnixStream::connect(&path).ok()?;
        stream.set_read_timeout(Some(wait)).ok()?;
        let mut fds = receive(&stream, 2).ok()?.into_iter();
        let (rw, ahead) = (fds.next()?, fds.next()?);
        Some((File::from(rw), File::from(ahead)))
    })
}

fn send(stream: &UnixStream, fds: &[RawFd]) -> io::Result<()> {
    let byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_ptr() as *mut libc::c_void,
        iov_len: 1,
    };
    let size = mem::size_of_val(fds) as u32;
    // SAFETY: CMSG_SPACE only computes a size.
    let mut space = vec![0u64; (unsafe { libc::CMSG_SPACE(size) } as usize).div_ceil(8)];
    // SAFETY: an all-zero msghdr is a valid empty one.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = space.as_mut_ptr().cast();
    msg.msg_controllen = (space.len() * 8) as _;
    // SAFETY: the control buffer is aligned and sized for one header carrying `fds`.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(size) as _;
        ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg).cast(), fds.len());
    }
    // SAFETY: msg points at buffers that outlive the call.
    match unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) } {
        sent if sent < 0 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

fn receive(stream: &UnixStream, count: usize) -> io::Result<Vec<OwnedFd>> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let size = (count * mem::size_of::<RawFd>()) as u32;
    // SAFETY: CMSG_SPACE only computes a size.
    let mut space = vec![0u64; (unsafe { libc::CMSG_SPACE(size) } as usize).div_ceil(8)];
    // SAFETY: an all-zero msghdr is a valid empty one.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = space.as_mut_ptr().cast();
    msg.msg_controllen = (space.len() * 8) as _;
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = 0;
    // SAFETY: msg points at buffers that outlive the call.
    if unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fds = Vec::new();
    // SAFETY: the headers walked are the kernel's, within the control buffer it filled, and each
    // descriptor it passed is ours to own from here.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let carried = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                    / mem::size_of::<RawFd>();
                let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                for at in 0..carried {
                    let fd = ptr::read_unaligned(data.add(at));
                    #[cfg(not(target_os = "linux"))]
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    match fds.len() == count {
        true => Ok(fds),
        false => Err(io::Error::other(
            "a member passed the wrong number of locks",
        )),
    }
}
