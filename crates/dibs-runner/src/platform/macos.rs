use crate::{
    platform::base::{Platform, Process, Slot},
    stop::Signals,
};
use std::{
    path::Path,
    process::{Command, Stdio},
};

/// macOS, read through libproc and the system's own tools.
pub struct MacOs;

fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_bsdinfo is plain data, and proc_pidinfo writes at most `size` bytes into it.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (read == size).then_some(info)
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
        bsd_info(pid).is_some()
    }

    fn running(pid: u32) -> bool {
        bsd_info(pid).is_some_and(|i| i.pbi_status != libc::SZOMB)
    }

    fn started_at(pid: u32) -> Option<u64> {
        bsd_info(pid).map(|i| i.pbi_start_tvsec)
    }

    fn group_of(pid: u32) -> Option<u32> {
        bsd_info(pid).map(|i| i.pbi_pgid)
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
                    parent: bsd_info(pid)?.pbi_ppid,
                })
            })
            .collect()
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
}
