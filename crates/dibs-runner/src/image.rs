//! A build run inside an image: an older distribution's root filesystem, pinned by digest, so
//! what the build links is that distribution's C library and not the machine's.

use crate::tree::Commands;
use std::{
    fs::{self, File, TryLockError},
    io,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

const DIGEST: usize = 64;
const TAG: usize = 12;
const IMAGES: &str = "images";
const USED: &str = ".dibs-used";
const LOCK_POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    reference: String,
    digest: String,
}

impl Image {
    /// `<ref>@sha256:<digest>`: a tag alone could name other files tomorrow.
    pub fn parse(text: &str) -> Result<Image, String> {
        text.rsplit_once("@sha256:")
            .map(|(_, digest)| digest)
            .filter(|digest| Image::hex(digest, DIGEST))
            .map(|digest| Image {
                reference: text.to_string(),
                digest: digest.to_string(),
            })
            .ok_or_else(|| {
                format!(
                    "dibs: the image {text:?} is not <ref>@sha256:<digest>, and only a digest pins what a build links\n"
                )
            })
    }

    /// Ends the name of every target built in this image, so no other build shares or seeds it.
    pub fn tag(&self) -> String {
        format!("-img{}", &self.digest[..TAG])
    }

    /// The image tag a target's name ends with, if it was built in one.
    pub fn tag_of(name: &str) -> Option<&str> {
        let at = name.rfind("-img")?;
        Image::hex(&name[at + 4..], TAG).then_some(&name[at..])
    }

    fn hex(text: &str, len: usize) -> bool {
        text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

/// The images unpacked on this machine, under its scratch.
pub struct Images<'a> {
    pub scratch: &'a Path,
    pub home: &'a Path,
}

impl Images<'_> {
    /// The image's root filesystem, fetched and unpacked the first time this machine needs it,
    /// within the job's cap and stopped with it.
    pub fn root(&self, image: &Image, commands: &Commands) -> Result<PathBuf, String> {
        let dir = self.scratch.join(IMAGES);
        let done = dir.join(&image.digest);
        let failed = |e: io::Error| format!("dibs: the image's directory {}: {e}\n", dir.display());
        fs::create_dir_all(&dir).map_err(failed)?;
        let lock = File::create(dir.join(format!(".{}.lock", image.digest))).map_err(failed)?;
        // Another job may be unpacking it; waited for only as long as this job may run.
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if !commands.deadline().passed() => {
                    thread::sleep(LOCK_POLL)
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(
                        "dibs: the job's cap ran out waiting for another job unpacking its image\n"
                            .into(),
                    );
                }
                Err(TryLockError::Error(e)) => return Err(failed(e)),
            }
        }
        if !done.join("rootfs").is_dir() {
            self.unpack(image, &dir, &done, commands)?;
        }
        let _ = File::create(done.join(USED));
        Ok(done.join("rootfs"))
    }

    fn unpack(
        &self,
        image: &Image,
        dir: &Path,
        done: &Path,
        commands: &Commands,
    ) -> Result<(), String> {
        let part = dir.join(format!(
            ".part-{}-{}",
            std::process::id(),
            &image.digest[..TAG]
        ));
        let unpacked = self.unpack_into(image, &part, done, commands);
        let _ = remove(&part);
        unpacked
    }

    fn unpack_into(
        &self,
        image: &Image,
        part: &Path,
        done: &Path,
        commands: &Commands,
    ) -> Result<(), String> {
        let _ = remove(part);
        fs::create_dir_all(part).map_err(|e| format!("dibs: {}: {e}\n", part.display()))?;
        let layout = format!("{}:image", part.join("oci").display());
        tool(
            commands,
            "skopeo",
            &[
                "copy",
                "--quiet",
                &format!("docker://{}", image.reference),
                &format!("oci:{layout}"),
            ],
        )?;
        let bundle = part.join("bundle");
        tool(
            commands,
            "umoci",
            &[
                "unpack",
                "--rootless",
                "--image",
                &layout,
                &bundle.display().to_string(),
            ],
        )?;
        let rootfs = bundle.join("rootfs");
        let prepared = self.mounts(&rootfs).and_then(|()| Account::add(&rootfs));
        prepared.map_err(|e| format!("dibs: the image's root filesystem: {e}\n"))?;
        fs::rename(&bundle, done).map_err(|e| format!("dibs: {}: {e}\n", done.display()))
    }

    /// The root is bound read-only, so what is bound over it needs its mount point made now.
    fn mounts(&self, rootfs: &Path) -> io::Result<()> {
        for dir in [self.home, self.scratch] {
            fs::create_dir_all(rootfs.join(dir.strip_prefix("/").unwrap_or(dir)))?;
        }
        Ok(())
    }

    /// The sandbox a command runs in: the image's root, this account's home and the machine's
    /// scratch at their own paths, so every cache dibs keeps is where the command expects it.
    pub fn sandbox(&self, rootfs: PathBuf) -> Sandbox {
        let mut binds = vec![self.home.to_path_buf()];
        if !self.scratch.starts_with(self.home) {
            binds.push(self.scratch.to_path_buf());
        }
        // A part of scratch moved to another disk is a link here, which must resolve inside.
        let moved: Vec<PathBuf> = fs::read_dir(self.scratch)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_symlink()))
                    .filter_map(|e| fs::canonicalize(e.path()).ok())
                    .filter(|real| real.is_dir() && !binds.iter().any(|b| real.starts_with(b)))
                    .collect()
            })
            .unwrap_or_default();
        binds.extend(moved);
        Sandbox {
            rootfs,
            binds,
            sealed: vec![self.scratch.join(IMAGES)],
        }
    }
}

/// The account dibs runs as, known inside the image, which ssh and git look up.
struct Account;

impl Account {
    fn add(rootfs: &Path) -> io::Result<()> {
        // SAFETY: getuid and getgid cannot fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        for (file, id, shell) in [("passwd", uid, true), ("group", gid, false)] {
            let host = fs::read_to_string(Path::new("/etc").join(file)).unwrap_or_default();
            let Some(line) = host
                .lines()
                .find(|l| l.split(':').nth(2) == Some(&id.to_string()))
            else {
                continue;
            };
            let line = match shell {
                // The host's login shell may not exist in the image.
                true => match line.rsplit_once(':') {
                    Some((head, _)) => format!("{head}:/bin/bash"),
                    None => line.to_string(),
                },
                false => line.to_string(),
            };
            let path = rootfs.join("etc").join(file);
            let mut text = fs::read_to_string(&path).unwrap_or_default();
            if !text.ends_with('\n') && !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&line);
            text.push('\n');
            fs::write(&path, text)?;
        }
        Ok(())
    }
}

/// How a command runs in an image: under bubblewrap, a child of the job like any command, so
/// the job's process group, its freeze and its descendants' CPU all still reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    rootfs: PathBuf,
    binds: Vec<PathBuf>,
    /// Bound read-only over the binds: the unpacked images every build shares.
    sealed: Vec<PathBuf>,
}

impl Sandbox {
    pub fn command(&self, command: &str) -> Command {
        let mut bwrap = Command::new("bwrap");
        bwrap.args(self.arguments(command));
        bwrap
    }

    fn arguments(&self, command: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "--ro-bind".into(),
            self.rootfs.display().to_string(),
            "/".into(),
            "--dev".into(),
            "/dev".into(),
            // Without its own PID namespace, an unprivileged bwrap cannot mount /proc.
            "--unshare-pid".into(),
            "--proc".into(),
            "/proc".into(),
            "--tmpfs".into(),
            "/tmp".into(),
        ];
        for file in ["/etc/resolv.conf", "/etc/hosts"] {
            if Path::new(file).exists() {
                args.extend(["--ro-bind".into(), file.into(), file.into()]);
            }
        }
        for dir in &self.binds {
            let dir = dir.display().to_string();
            args.extend(["--bind".into(), dir.clone(), dir]);
        }
        for dir in self.sealed.iter().filter(|d| d.is_dir()) {
            let dir = dir.display().to_string();
            args.extend(["--ro-bind".into(), dir.clone(), dir]);
        }
        args.extend(["--".into(), "bash".into(), "-c".into(), command.into()]);
        args
    }
}

fn tool(commands: &Commands, name: &str, args: &[&str]) -> Result<(), String> {
    let mut command = Command::new(name);
    command.args(args);
    let output = commands.output(command).map_err(|e| match e.kind() {
        io::ErrorKind::TimedOut => {
            format!("dibs: the job's cap ran out while {name} prepared its image\n")
        }
        io::ErrorKind::NotFound => format!(
            "dibs: building in an image needs skopeo, umoci and bwrap on this machine, and {name} is not installed\n"
        ),
        _ => format!("dibs: {name} could not start: {e}\n"),
    })?;
    match output.status.success() {
        true => Ok(()),
        false => Err(format!(
            "dibs: {name} {} failed ({}):\n{}",
            args.first().unwrap_or(&""),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}

/// Removes an unpacked tree, whose directories an image may have left read-only.
fn remove(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let _ = Command::new("chmod")
        .arg("-R")
        .arg("u+w")
        .arg(path)
        .status();
    fs::remove_dir_all(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn only_a_digest_names_an_image() {
        let image = Image::parse(&format!("docker.io/library/buildpack-deps@sha256:{DIGEST}"));
        assert_eq!(image.unwrap().tag(), "-img0123456789ab");
        assert!(Image::parse("docker.io/library/buildpack-deps:noble").is_err());
        assert!(Image::parse("x@sha256:0123").is_err());
        assert!(Image::parse(&format!("x@sha256:{}", DIGEST.to_uppercase())).is_err());
    }

    #[test]
    fn a_target_says_which_image_built_it() {
        assert_eq!(
            Image::tag_of("burn-local-k1-img0123456789ab"),
            Some("-img0123456789ab")
        );
        assert_eq!(Image::tag_of("burn-local-k1"), None);
        assert_eq!(Image::tag_of("burn-imgfoo"), None);
    }

    #[test]
    fn a_sandbox_binds_home_and_scratch_at_their_own_paths() {
        let images = Images {
            scratch: Path::new("/data/dibs"),
            home: Path::new("/home/dibs"),
        };
        let args = images
            .sandbox(PathBuf::from("/data/dibs/images/d/rootfs"))
            .arguments("cargo build");
        let joined = args.join(" ");
        assert!(joined.starts_with("--ro-bind /data/dibs/images/d/rootfs / "));
        assert!(joined.contains("--bind /home/dibs /home/dibs"));
        assert!(joined.contains("--bind /data/dibs /data/dibs"));
        assert!(joined.ends_with("-- bash -c cargo build"));
        let inside = Images {
            scratch: Path::new("/home/dibs/.cache/dibs"),
            home: Path::new("/home/dibs"),
        };
        let args = inside.sandbox(PathBuf::from("/r")).arguments("true");
        assert_eq!(args.iter().filter(|a| *a == "--bind").count(), 1);
    }

    #[test]
    fn a_sandbox_cannot_change_an_image_and_reaches_scratch_moved_elsewhere() {
        let root = std::env::temp_dir().join(format!("dibs-image.{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (scratch, disk) = (root.join("scratch"), root.join("disk"));
        fs::create_dir_all(scratch.join(IMAGES)).unwrap();
        fs::create_dir_all(disk.join("target")).unwrap();
        std::os::unix::fs::symlink(disk.join("target"), scratch.join("target")).unwrap();
        let home = root.join("home");
        let images = Images {
            scratch: &scratch,
            home: &home,
        };
        let joined = images
            .sandbox(PathBuf::from("/r"))
            .arguments("true")
            .join(" ");
        let real = fs::canonicalize(disk.join("target"))
            .unwrap()
            .display()
            .to_string();
        assert!(
            joined.contains(&format!("--bind {real} {real}")),
            "{joined}"
        );
        let sealed = scratch.join(IMAGES).display().to_string();
        assert!(
            joined.contains(&format!("--ro-bind {sealed} {sealed} --")),
            "after every writable bind: {joined}"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
