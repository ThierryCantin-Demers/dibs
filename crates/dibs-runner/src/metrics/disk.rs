use crate::{
    machine::Site,
    platform::{Host, Platform as _},
    tree::Free,
};
use dibs_format::status::{Disk, DiskRole, Quota};
use std::{
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

/// Each filesystem a job writes to, once: scratch, the `TMPDIR` jobs are given, the system's
/// `/tmp`, which is what a tool ignoring `TMPDIR` fills, and wherever scratch's own entries were
/// moved to. Read by statvfs, which is exact where summing files is not: reflinked trees share
/// their blocks.
pub fn disks(machine: &Site) -> Vec<Disk> {
    let scratch = &machine.scratch;
    let device = |dir: &Path| std::fs::metadata(dir).ok().map(|m| m.dev());
    let home = device(scratch);
    let mut entries: Vec<(String, PathBuf, u64)> = std::fs::read_dir(scratch)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let dev = device(&path).filter(|d| Some(*d) != home)?;
            Some((entry.file_name().to_string_lossy().into_owned(), path, dev))
        })
        .collect();
    entries.sort();
    let linked = entries
        .iter()
        .map(|(_, path, _)| (DiskRole::Linked, path.clone()));
    let mut seen = Vec::new();
    [
        (DiskRole::Scratch, scratch.clone()),
        (DiskRole::Tmp, machine.tmp()),
        (DiskRole::SystemTmp, "/tmp".into()),
    ]
    .into_iter()
    .chain(linked)
    .filter_map(|(role, dir): (DiskRole, PathBuf)| {
        let dir = dir.canonicalize().ok()?;
        let device = device(&dir)?;
        if seen.contains(&device) {
            return None;
        }
        seen.push(device);
        let free = Free::of(&dir)?;
        Some(Disk {
            role,
            mount: free.mount.display().to_string(),
            fstype: Host::filesystem(&dir),
            total: free.size,
            free: free.available,
            quotas: quotas(&dir),
            paths: entries
                .iter()
                .filter(|(_, _, on)| *on == device)
                .map(|(name, _, _)| name.clone())
                .collect(),
        })
    })
    .collect()
}

#[cfg(not(target_os = "linux"))]
fn quotas(_: &Path) -> Vec<Quota> {
    Vec::new()
}

/// Every quota on the filesystem that counts what this account writes in `dir`: its user's, its
/// group's, and the project `dir` is in. Only those with a limit.
#[cfg(target_os = "linux")]
fn quotas(dir: &Path) -> Vec<Quota> {
    use dibs_format::status::QuotaKind;
    use std::{fs::File, os::fd::AsRawFd as _};

    let Ok(file) = File::open(dir) else {
        return Vec::new();
    };
    let fd = file.as_raw_fd();
    // SAFETY: getuid and getgid cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let mut out = Vec::new();
    for (kind, id) in [
        (QuotaKind::User, Some(uid)),
        (QuotaKind::Group, Some(gid)),
        (QuotaKind::Project, project(fd).filter(|p| *p != 0)),
    ] {
        let Some(id) = id else { continue };
        if let Some(quota) = linux::quota(fd, kind, id) {
            out.push(quota);
        }
    }
    out
}

/// The project a directory is in, which XFS and ext4 project quotas count it under.
#[cfg(target_os = "linux")]
fn project(fd: std::os::fd::RawFd) -> Option<u32> {
    #[repr(C)]
    struct FsXattr {
        xflags: u32,
        extsize: u32,
        nextents: u32,
        projid: u32,
        cowextsize: u32,
        pad: [u8; 8],
    }
    const FS_IOC_FSGETXATTR: libc::c_ulong = 0x801c_581f;
    // SAFETY: plain data, which the ioctl fills on success.
    let mut attr: FsXattr = unsafe { std::mem::zeroed() };
    (unsafe { libc::ioctl(fd, FS_IOC_FSGETXATTR, &mut attr) } == 0).then_some(attr.projid)
}

#[cfg(target_os = "linux")]
mod linux {
    use dibs_format::status::{Quota, QuotaKind};
    use std::os::fd::RawFd;

    /// `struct if_dqblk`: limits in 1 KiB blocks, use in bytes.
    #[repr(C)]
    struct DqBlk {
        bhardlimit: u64,
        bsoftlimit: u64,
        curspace: u64,
        ihardlimit: u64,
        isoftlimit: u64,
        curinodes: u64,
        btime: u64,
        itime: u64,
        valid: u32,
    }
    const Q_GETQUOTA: libc::c_int = 0x80_0007;

    pub fn quota(fd: RawFd, kind: QuotaKind, id: u32) -> Option<Quota> {
        let kind_bits = match kind {
            QuotaKind::User => 0,
            QuotaKind::Group => 1,
            QuotaKind::Project => 2,
        };
        // SAFETY: plain data, which quotactl fills on success.
        let mut found: DqBlk = unsafe { std::mem::zeroed() };
        let asked = unsafe {
            libc::syscall(
                libc::SYS_quotactl_fd,
                fd,
                (Q_GETQUOTA << 8) | kind_bits,
                id,
                &mut found as *mut DqBlk,
            )
        };
        let limit = match found.bhardlimit {
            0 => found.bsoftlimit,
            hard => hard,
        };
        (asked == 0 && limit > 0).then(|| Quota {
            kind,
            id,
            limit: limit * 1024,
            used: found.curspace,
        })
    }
}
