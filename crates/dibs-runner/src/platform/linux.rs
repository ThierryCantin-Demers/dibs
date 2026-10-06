use crate::{
    platform::base::{Extent, Platform, Process, Slot, elapsed},
    settings::Settings,
};
use std::{
    ffi::CString,
    fs,
    os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _, unix::fs::MetadataExt as _},
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// Linux, read from `/proc` and `/sys`.
pub struct Linux;

/// `statfs`'s magic numbers for the filesystems whose files share blocks.
const XFS: u32 = 0x5846_5342;
const BTRFS: u32 = 0x9123_683e;
/// `_IOW(0x94, 9, int)`.
const FICLONE: libc::Ioctl = 0x4004_9409;
/// `_IOWR('f', 11, struct fiemap)`.
const FS_IOC_FIEMAP: libc::Ioctl = 0xc020_660b;
const FIEMAP_EXTENT_LAST: u32 = 0x1;
const FIEMAP_EXTENT_SHARED: u32 = 0x2000;
/// Extents asked for at a time.
const EXTENTS: usize = 64;

/// `<linux/fiemap.h>`'s request, with room for `EXTENTS` answers.
#[repr(C)]
struct Fiemap {
    start: u64,
    length: u64,
    flags: u32,
    mapped: u32,
    count: u32,
    reserved: u32,
    extents: [FiemapExtent; EXTENTS],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FiemapExtent {
    logical: u64,
    physical: u64,
    length: u64,
    reserved64: [u64; 2],
    flags: u32,
    reserved: [u32; 3],
}

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

    fn fd_path(pid: u32, fd: u32) -> Option<String> {
        fs::read_link(format!("/proc/{pid}/fd/{fd}"))
            .ok()
            .map(|p| p.display().to_string())
    }

    fn cwd(pid: u32) -> Option<PathBuf> {
        fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    fn variable(pid: u32, name: &str) -> Option<String> {
        let environment = fs::read(format!("/proc/{pid}/environ")).ok()?;
        environment.split(|b| *b == 0).find_map(|pair| {
            let value = pair.strip_prefix(name.as_bytes())?.strip_prefix(b"=")?;
            Some(String::from_utf8_lossy(value).into_owned())
        })
    }

    fn listening() -> Vec<u16> {
        ["/proc/net/tcp", "/proc/net/tcp6"]
            .iter()
            .filter_map(|table| fs::read_to_string(table).ok())
            .flat_map(|table| {
                table
                    .lines()
                    .skip(1)
                    .filter_map(|line| {
                        let fields: Vec<&str> = line.split_whitespace().collect();
                        let port = fields.get(1)?.rsplit(':').next()?;
                        (fields.get(3) == Some(&"0A"))
                            .then(|| u16::from_str_radix(port, 16).ok())
                            .flatten()
                    })
                    .collect::<Vec<u16>>()
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

    fn children(pid: u32) -> Option<Vec<u32>> {
        if !lists_children() {
            return None;
        }
        let tasks = fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten();
        Some(
            tasks
                .flatten()
                .filter_map(|task| fs::read_to_string(task.path().join("children")).ok())
                .flat_map(|list| {
                    list.split_whitespace()
                        .filter_map(|p| p.parse().ok())
                        .collect::<Vec<u32>>()
                })
                .collect(),
        )
    }

    fn cpu_ticks(pid: u32) -> Option<u64> {
        let stat = Stat::of(pid)?;
        (14..=17).map(|field| stat.number(field)).sum()
    }

    fn clock_ticks() -> u64 {
        clock_ticks()
    }

    fn describe(pid: u32) -> Option<String> {
        let started = Linux::started_at(pid)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let uid = fs::metadata(format!("/proc/{pid}")).ok()?.uid();
        let args = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let args = String::from_utf8_lossy(&args)
            .split('\0')
            .filter(|a| !a.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let args = match args.is_empty() {
            true => format!(
                "[{}]",
                first_line(format!("/proc/{pid}/comm")).unwrap_or_default()
            ),
            false => args,
        };
        Some(format!(
            "{pid} {} {} {args}",
            elapsed(now.saturating_sub(started)),
            user_name(uid)
        ))
    }

    fn shares_blocks(dir: &Path) -> bool {
        let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: statfs is plain data, which statfs fills from a NUL-terminated path.
        let mut found: libc::statfs = unsafe { std::mem::zeroed() };
        let read = unsafe { libc::statfs(path.as_ptr(), &mut found) };
        read == 0 && matches!(found.f_type as u32, XFS | BTRFS)
    }

    fn reflink(from: &Path, to: &Path) -> bool {
        let (Ok(source), Ok(copy)) = (fs::File::open(from), fs::File::create(to)) else {
            return false;
        };
        // SAFETY: FICLONE reads the source descriptor and fills the copy's, both open here.
        unsafe { libc::ioctl(copy.as_raw_fd(), FICLONE, source.as_raw_fd()) == 0 }
    }

    /// The longest mount point above the directory, as `/proc/self/mounts` lists them.
    fn filesystem(dir: &Path) -> Option<String> {
        let dir = dir.canonicalize().ok()?;
        let mounts = fs::read_to_string("/proc/self/mounts").ok()?;
        mounts
            .lines()
            .filter_map(|line| {
                let mut fields = line.split(' ');
                let point = fields.nth(1)?.replace("\\040", " ");
                let kind = fields.next()?;
                dir.starts_with(&point)
                    .then(|| (point.len(), kind.to_string()))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, kind)| kind)
    }

    fn cpu_model() -> Option<String> {
        fs::read_to_string("/proc/cpuinfo")
            .ok()?
            .lines()
            .find_map(|l| {
                Some(
                    l.strip_prefix("model name")?
                        .split_once(": ")?
                        .1
                        .to_string(),
                )
            })
    }

    fn on_battery() -> bool {
        fs::read_dir("/sys/class/power_supply")
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("BAT"))
    }

    /// A pipe's write end polled for nothing wakes only with an error once its reader has gone.
    fn await_caller_gone() {
        let mut fd = libc::pollfd {
            fd: libc::STDOUT_FILENO,
            events: 0,
            revents: 0,
        };
        let gone = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
        // SAFETY: one pollfd, owned here; with no events asked for, only a hangup wakes it.
        while unsafe { libc::poll(&mut fd, 1, -1) } < 1 || fd.revents & gone == 0 {}
    }

    fn extents(file: &Path) -> Option<Vec<Extent>> {
        let opened = fs::File::open(file).ok()?;
        let mut extents = Vec::new();
        let mut start = 0;
        let mut done = false;
        while !done {
            let mapped = Linux::map_from(&opened, start)?;
            done = mapped.is_empty();
            for extent in mapped {
                extents.push(Extent {
                    physical: extent.physical,
                    length: extent.length,
                    shared: extent.flags & FIEMAP_EXTENT_SHARED != 0,
                });
                start = extent.logical + extent.length;
                done |= extent.flags & FIEMAP_EXTENT_LAST != 0;
            }
        }
        Some(extents)
    }
}

impl Linux {
    /// One FIEMAP request from `start`: the extents mapped, the last flagged so.
    fn map_from(file: &fs::File, start: u64) -> Option<Vec<FiemapExtent>> {
        // SAFETY: Fiemap is plain data; FS_IOC_FIEMAP fills at most `count` extents of it.
        let mut map: Fiemap = unsafe { std::mem::zeroed() };
        map.start = start;
        map.length = u64::MAX - start;
        map.count = EXTENTS as u32;
        let asked = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP, &mut map) };
        (asked == 0).then(|| map.extents[..(map.mapped as usize).min(EXTENTS)].to_vec())
    }
}

/// Whether the kernel lists a process's children, which one built without
/// `CONFIG_PROC_CHILDREN` cannot; `DIBS_NO_CHILDREN=1` reads the whole table instead.
fn lists_children() -> bool {
    static LISTS: OnceLock<bool> = OnceLock::new();
    *LISTS.get_or_init(|| {
        let me = std::process::id();
        !Settings::load().no_children
            && Path::new(&format!("/proc/{me}/task/{me}/children")).exists()
    })
}

/// The account's name, or its number where it has none.
fn user_name(uid: u32) -> String {
    let mut buffer = vec![0 as libc::c_char; 4096];
    // SAFETY: passwd is plain data; getpwuid_r fills it and the buffer, both owned here.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let read = unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if read != 0 || found.is_null() {
        return uid.to_string();
    }
    // SAFETY: getpwuid_r left pw_name pointing at a NUL-terminated name in the buffer.
    unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }
        .to_string_lossy()
        .into_owned()
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
        assert!(Linux::children(me).is_some());
        assert!(Linux::describe(me).is_some_and(|d| d.starts_with(&format!("{me} "))));
    }

    #[test]
    fn a_reflinked_copy_shares_the_blocks_of_its_original() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        if !Linux::shares_blocks(&dir) {
            eprintln!("skipped: {} cannot share blocks", dir.display());
            return;
        }
        let original = dir.join(format!("extents-{}", std::process::id()));
        let copy = original.with_extension("copy");
        let mut file = fs::File::create(&original).unwrap();
        std::io::Write::write_all(&mut file, &vec![7u8; 256 * 1024]).unwrap();
        file.sync_all().unwrap();
        let own = Linux::extents(&original).unwrap();
        assert!(own.iter().all(|e| !e.shared));
        assert!(own.iter().map(|e| e.length).sum::<u64>() >= 256 * 1024);
        let copied = std::process::Command::new("cp")
            .arg("--reflink=always")
            .arg(&original)
            .arg(&copy)
            .status()
            .is_ok_and(|s| s.success());
        if copied {
            assert!(Linux::extents(&copy).unwrap().iter().all(|e| e.shared));
        }
        let _ = fs::remove_file(&original);
        let _ = fs::remove_file(&copy);
    }
}
