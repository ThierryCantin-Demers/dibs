use crate::{
    platform::base::{Extent, Platform, Process, Slot},
    stop::Signals,
};
use std::{
    ffi::{CStr, CString},
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// macOS, read through libproc and the system's own tools.
pub struct MacOs;

/// Mach time units to nanoseconds, as `numer / denom`; libc's binding is deprecated.
#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
}

/// `<sys/proc_info.h>`'s flavor and layout for a descriptor's path, which libc does not carry.
const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

#[repr(C)]
struct FileInfo {
    _open_flags: u32,
    _status: u32,
    _offset: libc::off_t,
    _kind: i32,
    _guard_flags: u32,
}

#[repr(C)]
struct VnodeWithPath {
    _file: FileInfo,
    vnode: libc::vnode_info_path,
}

/// Bytes enough for a `struct kinfo_proc`, 648 on both architectures.
const KINFO_PROC_ROOM: usize = 1024;

impl MacOs {
    /// What any account may read of any process, a zombie included. `PROC_PIDTBSDINFO` reads
    /// only this account's, which would take another account's live job for one gone.
    fn short_info(pid: u32) -> Option<libc::proc_bsdshortinfo> {
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        // SAFETY: proc_bsdshortinfo is plain data, and proc_pidinfo writes at most `size` bytes.
        let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let read = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdshortinfo).cast(),
                size,
            )
        };
        (read == size).then_some(info)
    }

    /// When it started, which the short info leaves out: `sysctl(KERN_PROC_PID)` answers any
    /// account, and its `kinfo_proc` begins with `p_starttime`, whose seconds are an `i64`.
    fn start_time(pid: u32) -> Option<u64> {
        let mut name = [
            libc::CTL_KERN,
            libc::KERN_PROC,
            libc::KERN_PROC_PID,
            pid as libc::c_int,
        ];
        let mut answer = [0u8; KINFO_PROC_ROOM];
        let mut length = answer.len();
        // SAFETY: sysctl writes at most `length` bytes into the buffer, and says how many.
        let failed = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                name.len() as libc::c_uint,
                answer.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        } != 0;
        let seconds = answer.get(..8).filter(|_| !failed && length >= 16)?;
        u64::try_from(i64::from_ne_bytes(seconds.try_into().ok()?)).ok()
    }
}

/// What a tool printed, empty when it could not run.
fn output_of(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

impl Platform for MacOs {
    fn exists(pid: u32) -> bool {
        MacOs::short_info(pid).is_some()
    }

    fn running(pid: u32) -> bool {
        MacOs::short_info(pid).is_some_and(|i| i.pbsi_status != libc::SZOMB)
    }

    fn started_at(pid: u32) -> Option<u64> {
        MacOs::start_time(pid)
    }

    fn group_of(pid: u32) -> Option<u32> {
        MacOs::short_info(pid).map(|i| i.pbsi_pgid)
    }

    fn processes() -> Vec<Process> {
        // SAFETY: a null buffer asks only for the count.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let Ok(count) = usize::try_from(count) else {
            return Vec::new();
        };
        let mut pids = vec![0 as libc::c_int; count + 64];
        let bytes = (pids.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int;
        // SAFETY: the buffer holds `bytes` bytes.
        let found = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        pids.truncate(usize::try_from(found).unwrap_or_default());
        pids.into_iter()
            .filter_map(|pid| {
                let pid = u32::try_from(pid).ok()?;
                Some(Process {
                    pid,
                    parent: MacOs::short_info(pid)?.pbsi_ppid,
                })
            })
            .collect()
    }

    /// Through libproc, so a status costs no process per descriptor; a pipe or a socket has no
    /// path to give.
    /// Not read: with no /proc, a tree in use here is told by its marker's age alone.
    fn cwd(_pid: u32) -> Option<PathBuf> {
        None
    }

    fn fd_path(pid: u32, fd: u32) -> Option<String> {
        let size = std::mem::size_of::<VnodeWithPath>() as libc::c_int;
        // SAFETY: VnodeWithPath is plain data, and proc_pidfdinfo writes at most `size` bytes.
        let mut info: VnodeWithPath = unsafe { std::mem::zeroed() };
        let read = unsafe {
            libc::proc_pidfdinfo(
                pid as libc::c_int,
                fd as libc::c_int,
                PROC_PIDFDVNODEPATHINFO,
                (&mut info as *mut VnodeWithPath).cast(),
                size,
            )
        };
        if read != size {
            return None;
        }
        let path: Vec<u8> = info
            .vnode
            .vip_path
            .iter()
            .flatten()
            .map(|c| *c as u8)
            .take_while(|b| *b != 0)
            .collect();
        Some(String::from_utf8_lossy(&path).into_owned())
    }

    fn listening() -> Vec<u16> {
        output_of("netstat", &["-anp", "tcp"])
            .lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                (fields.get(5) == Some(&"LISTEN"))
                    .then(|| fields.get(3)?.rsplit('.').next()?.parse().ok())
                    .flatten()
            })
            .collect()
    }

    /// Which process holds an flock is visible only in Linux's `/proc`.
    fn lock_holders(_file: &Path) -> Vec<u32> {
        Vec::new()
    }

    fn slot(_pci: &str) -> Slot {
        Slot::Unreadable
    }

    /// Without a fan, a Mac's number depends on its power source and on whether it throttles.
    fn machine_state() -> String {
        let power = output_of("pmset", &["-g", "ps"])
            .lines()
            .next()
            .and_then(|l| l.split_once('\'')?.1.split_once(" Power'"))
            .map(|(source, _)| source.to_ascii_lowercase())
            .unwrap_or_default();
        let thermal = output_of("pmset", &["-g", "therm"])
            .lines()
            .find_map(|l| {
                let lower = l.to_ascii_lowercase();
                let after = &lower[lower.find("warning level")? + "warning level".len()..];
                let digits: String = after
                    .chars()
                    .skip_while(|c| !c.is_ascii_digit())
                    .take_while(char::is_ascii_digit)
                    .collect();
                (!digits.is_empty()).then_some(digits)
            })
            .unwrap_or_else(|| "nominal".into());
        let low_power: Vec<String> = output_of("pmset", &["-g"])
            .lines()
            .filter(|l| l.contains("lowpowermode"))
            .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
            .collect();
        format!(
            "kernel={} macos={} power={power} lowpower={} thermal={thermal}",
            output_of("uname", &["-r"]).trim(),
            output_of("sw_vers", &["-productVersion"]).trim(),
            low_power.join("\n")
        )
    }

    /// A Mac sleeps when nobody has touched it, however busy it is, and a job it sleeps through
    /// is lost with its ssh.
    fn stay_awake(pid: u32) {
        let mut caffeinate = Command::new("caffeinate");
        caffeinate
            .args(["-i", "-w", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Signals::unblocked(&mut caffeinate);
        let _ = caffeinate.spawn();
    }

    fn children(pid: u32) -> Option<Vec<u32>> {
        let mut pids = vec![0 as libc::pid_t; 4096];
        let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
        // SAFETY: the buffer holds `bytes` bytes.
        let found = unsafe {
            libc::proc_listchildpids(pid as libc::pid_t, pids.as_mut_ptr().cast(), bytes)
        };
        let found = usize::try_from(found).ok()?;
        pids.truncate(found.min(pids.len()));
        Some(
            pids.into_iter()
                .filter_map(|p| u32::try_from(p).ok())
                .filter(|p| *p > 0)
                .collect(),
        )
    }

    /// Reaped children's time is in the process's own rusage, in Mach time units.
    fn cpu_ticks(pid: u32) -> Option<u64> {
        // SAFETY: rusage_info_v2 is plain data, which proc_pid_rusage fills.
        let mut usage: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let read = unsafe {
            libc::proc_pid_rusage(
                pid as libc::c_int,
                libc::RUSAGE_INFO_V2,
                (&mut usage as *mut libc::rusage_info_v2).cast(),
            )
        };
        if read != 0 {
            return None;
        }
        let mut base = Timebase { numer: 1, denom: 1 };
        // SAFETY: mach_timebase_info fills the struct it is given.
        unsafe { mach_timebase_info(&mut base) };
        let units = u128::from(usage.ri_user_time)
            + u128::from(usage.ri_system_time)
            + u128::from(usage.ri_child_user_time)
            + u128::from(usage.ri_child_system_time);
        let nanos = units * u128::from(base.numer.max(1)) / u128::from(base.denom.max(1));
        u64::try_from(nanos * u128::from(MacOs::clock_ticks()) / 1_000_000_000).ok()
    }

    fn clock_ticks() -> u64 {
        // SAFETY: sysconf only reads a configuration value.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100)
    }

    /// APFS clones share blocks too, but nothing here asks which.
    fn shares_blocks(_dir: &Path) -> bool {
        false
    }

    fn extents(_file: &Path) -> Option<Vec<Extent>> {
        None
    }

    fn reflink(_from: &Path, _to: &Path) -> bool {
        false
    }

    fn filesystem(dir: &Path) -> Option<String> {
        let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: statfs is plain data, which statfs fills from a NUL-terminated path.
        let mut found: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(path.as_ptr(), &mut found) } != 0 {
            return None;
        }
        // SAFETY: statfs leaves f_fstypename NUL-terminated.
        let name = unsafe { CStr::from_ptr(found.f_fstypename.as_ptr()) };
        Some(name.to_string_lossy().into_owned())
    }

    fn cpu_model() -> Option<String> {
        let model = output_of("sysctl", &["-n", "machdep.cpu.brand_string"]);
        Some(model.trim().to_string()).filter(|m| !m.is_empty())
    }

    fn on_battery() -> bool {
        output_of("pmset", &["-g", "batt"]).contains("InternalBattery")
    }

    /// A pipe polled for nothing never wakes here, so the parent's exit is what is waited on: the
    /// process ssh started for the call, or the client on this computer.
    fn await_caller_gone() {
        // SAFETY: getppid cannot fail.
        let parent = unsafe { libc::getppid() };
        if parent <= 1 {
            return;
        }
        // SAFETY: one kqueue, watching one pid for its exit; a parent already gone comes back
        // as an event flagged EV_ERROR, which is as much an answer.
        unsafe {
            let queue = libc::kqueue();
            if queue < 0 {
                return;
            }
            let change = libc::kevent {
                ident: parent as usize,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            let mut event = change;
            while libc::kevent(queue, &change, 1, &mut event, 1, std::ptr::null()) < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
            }
            libc::close(queue);
        }
    }

    /// Asked of ps, which costs a process; only Linux ever finds an orphan to describe.
    fn describe(pid: u32) -> Option<String> {
        let said = output_of(
            "ps",
            &["-o", "pid=,etime=,user=,args=", "-p", &pid.to_string()],
        );
        let said = said.lines().next()?.trim().to_string();
        (!said.is_empty()).then_some(said)
    }
}
