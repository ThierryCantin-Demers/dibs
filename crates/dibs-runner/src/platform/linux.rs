use crate::platform::base::{Platform, Process, Slot};
use std::{
    fs,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// Linux, read from `/proc` and `/sys`.
pub struct Linux;

/// The fields of `/proc/<pid>/stat` after the command, which may hold spaces and parentheses.
struct Stat {
    fields: Vec<String>,
}

impl Stat {
    fn of(pid: u32) -> Option<Stat> {
        let line = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, rest) = line.rsplit_once(") ")?;
        Some(Stat {
            fields: rest.split_whitespace().map(str::to_string).collect(),
        })
    }

    /// The field `proc(5)` numbers `n`, counting the pid as 1.
    fn field(&self, n: usize) -> Option<&str> {
        self.fields.get(n.checked_sub(3)?).map(String::as_str)
    }

    fn number(&self, n: usize) -> Option<u64> {
        self.field(n)?.parse().ok()
    }
}

/// When the machine booted, in seconds since the epoch.
fn boot_time() -> Option<u64> {
    static BOOT: OnceLock<Option<u64>> = OnceLock::new();
    *BOOT.get_or_init(|| {
        fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()
    })
}

fn clock_ticks() -> u64 {
    // SAFETY: sysconf only reads a configuration value.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100)
}

fn pids() -> impl Iterator<Item = u32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
}

fn first_line(path: impl AsRef<Path>) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    Some(text.lines().next().unwrap_or_default().trim().to_string())
}

/// `uname -r`.
fn kernel_release() -> String {
    // SAFETY: utsname is plain data, and uname fills it.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return String::new();
    }
    // SAFETY: uname leaves release NUL-terminated.
    unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// The first `N.N` or `N.N.N` in the text.
fn first_version(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    (0..bytes.len()).find_map(|start| {
        if start > 0 && bytes[start - 1].is_ascii_digit() {
            return None;
        }
        let mut end = start;
        let mut parts = 0;
        while parts < 3 {
            let digits = bytes[end..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if digits == 0 {
                break;
            }
            end += digits;
            parts += 1;
            match bytes.get(end) {
                Some(b'.') if parts < 3 && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) => {
                    end += 1
                }
                _ => break,
            }
        }
        (parts >= 2).then(|| text[start..end].to_string())
    })
}

impl Platform for Linux {
    fn exists(pid: u32) -> bool {
        Path::new(&format!("/proc/{pid}")).is_dir()
    }

    fn running(pid: u32) -> bool {
        Stat::of(pid).is_some_and(|s| s.field(3).is_some_and(|state| state != "Z"))
    }

    fn started_at(pid: u32) -> Option<u64> {
        let since_boot = Stat::of(pid)?.number(22)?;
        Some(boot_time()? + since_boot / clock_ticks())
    }

    fn group_of(pid: u32) -> Option<u32> {
        Stat::of(pid)?.field(5)?.parse().ok()
    }

    fn processes() -> Vec<Process> {
        pids()
            .filter_map(|pid| {
                let parent = Stat::of(pid)?.field(4)?.parse().ok()?;
                Some(Process { pid, parent })
            })
            .collect()
    }

    fn lock_holders(file: &Path) -> Vec<u32> {
        let Ok(lock) = fs::metadata(file) else {
            return Vec::new();
        };
        let marker = format!(":{} ", lock.ino());
        let holds = |pid: u32, fd: &str| {
            fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")).is_ok_and(|info| {
                info.lines()
                    .any(|l| l.starts_with("lock:") && l.contains(&marker))
            })
        };
        pids()
            .filter(|&pid| {
                let fds = PathBuf::from(format!("/proc/{pid}/fd"));
                fs::read_dir(&fds)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .any(|fd| {
                        fs::metadata(fd.path())
                            .is_ok_and(|m| m.ino() == lock.ino() && m.dev() == lock.dev())
                            && holds(pid, &fd.file_name().to_string_lossy())
                    })
            })
            .collect()
    }

    fn slot(pci: &str) -> Slot {
        let devices = Path::new("/sys/bus/pci/devices");
        if !devices.is_dir() {
            return Slot::Unreadable;
        }
        let ids: Vec<String> = ["vendor", "device"]
            .iter()
            .filter_map(|f| first_line(devices.join(pci).join(f)))
            .map(|id| id.trim_start_matches("0x").to_ascii_lowercase())
            .collect();
        match ids.is_empty() {
            true => Slot::Empty,
            false => Slot::Holds(ids.join(":")),
        }
    }

    fn machine_state() -> String {
        let mut governors: Vec<String> = fs::read_dir("/sys/devices/system/cpu")
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("cpu"))
            .filter_map(|e| first_line(e.path().join("cpufreq/scaling_governor")))
            .collect();
        governors.sort();
        governors.dedup();
        let nvidia = fs::read_to_string("/proc/driver/nvidia/version")
            .ok()
            .and_then(|v| first_version(&v))
            .unwrap_or_default();
        format!(
            "governor={} kernel={} nvidia={nvidia}",
            governors.join("+"),
            kernel_release()
        )
    }

    fn stay_awake(_pid: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_driver_version_is_its_first_dotted_number() {
        let line = "NVRM version: NVIDIA UNIX x86_64 Kernel Module  550.54.14  Thu Feb 22 01:44:30 UTC 2024";
        assert_eq!(first_version(line).as_deref(), Some("550.54.14"));
        assert_eq!(first_version("v2 and 3.10").as_deref(), Some("3.10"));
        assert_eq!(first_version("1.2.3.4").as_deref(), Some("1.2.3"));
        assert_eq!(first_version("none here"), None);
    }

    #[test]
    fn this_process_is_running_and_started_before_now() {
        let me = std::process::id();
        assert!(Linux::exists(me) && Linux::running(me));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(Linux::started_at(me).is_some_and(|s| s <= now + 1));
        assert!(
            Linux::processes()
                .iter()
                .any(|p| p.pid == me && p.parent == std::os::unix::process::parent_id())
        );
    }
}
