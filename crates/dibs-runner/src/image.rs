//! A build run inside an image: an older distribution's root filesystem, pinned by digest, so
//! what the build links is that distribution's C library and not the machine's.

use crate::tree::Commands;
use std::{
    fs::{self, File, TryLockError},
    io,
    path::{Component, Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

const DIGEST: usize = 64;
const TAG: usize = 12;
const IMAGES: &str = "images";
const USED: &str = ".dibs-used";
const LOCK_POLL: Duration = Duration::from_millis(200);
/// What resolving a name needs from the host.
const NETWORK: [&str; 2] = ["/etc/resolv.conf", "/etc/hosts"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    reference: String,
    digest: String,
}

impl Image {
    /// `<ref>@sha256:<digest>`: a tag alone could name other files tomorrow.
    pub fn parse(text: &str) -> Result<Image, String> {
        text.rsplit_once("@sha256:")
            .filter(|(name, _)| {
                !Image::repository(name).is_empty()
                    && !name.contains(|c: char| c.is_whitespace() || c == '@')
            })
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

    /// What a registry is asked for: the name and digest, without the tag a reader keeps it by,
    /// since skopeo refuses a reference with both.
    fn pinned(&self) -> String {
        let (name, _) = self.reference.rsplit_once('@').unwrap_or_default();
        format!("{}@sha256:{}", Image::repository(name), self.digest)
    }

    /// A name without its tag; a colon before the last slash is a registry's port.
    fn repository(name: &str) -> &str {
        match name.rfind(':') {
            Some(colon) if !name[colon..].contains('/') => &name[..colon],
            _ => name,
        }
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

    /// A target's name without the image tag it ends with.
    pub fn untagged(name: &str) -> &str {
        Image::tag_of(name).map_or(name, |tag| &name[..name.len() - tag.len()])
    }

    fn hex(text: &str, len: usize) -> bool {
        text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

/// Why an image is not ready to build in.
#[derive(Debug)]
pub struct Unready {
    pub why: String,
    /// The job's cap ran out preparing it.
    pub overran: bool,
}

impl From<String> for Unready {
    fn from(why: String) -> Unready {
        Unready {
            why,
            overran: false,
        }
    }
}

fn overran(why: String) -> Unready {
    Unready { why, overran: true }
}

/// The images unpacked on this machine, under its scratch.
pub struct Images<'a> {
    pub scratch: &'a Path,
    pub home: &'a Path,
}

impl Images<'_> {
    /// The image's root filesystem, fetched and unpacked the first time this machine needs it,
    /// within the job's cap and stopped with it.
    /// The sandbox a build runs in, its mount points made under the image's lock, since every
    /// build of the image makes them in the same root.
    pub fn prepare(&self, image: &Image, commands: &Commands) -> Result<Sandbox, Unready> {
        let (rootfs, _lock) = self.root(image, commands)?;
        self.sandbox(rootfs)
    }

    fn root(&self, image: &Image, commands: &Commands) -> Result<(PathBuf, File), Unready> {
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
                    return Err(overran(
                        "dibs: the job's cap ran out waiting for another job unpacking its image\n"
                            .into(),
                    ));
                }
                Err(TryLockError::Error(e)) => return Err(failed(e).into()),
            }
        }
        // Under the image's lock, any part of it left by a job stopped mid-unpack is abandoned.
        let suffix = format!("-{}", &image.digest[..TAG]);
        for left in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = left.file_name().to_string_lossy().into_owned();
            if name.starts_with(".part-") && name.ends_with(&suffix) {
                remove(commands, &left.path())?;
            }
        }
        if !done.join("rootfs").is_dir() {
            self.unpack(image, &dir, &done, commands)?;
        }
        let _ = File::create(done.join(USED));
        Ok((done.join("rootfs"), lock))
    }

    fn unpack(
        &self,
        image: &Image,
        dir: &Path,
        done: &Path,
        commands: &Commands,
    ) -> Result<(), Unready> {
        let part = dir.join(format!(
            ".part-{}-{}",
            std::process::id(),
            &image.digest[..TAG]
        ));
        let unpacked = self.unpack_into(image, &part, done, commands);
        match remove(commands, &part) {
            Err(unready) if unready.overran => Err(unready),
            removed => unpacked.and(removed),
        }
    }

    fn unpack_into(
        &self,
        image: &Image,
        part: &Path,
        done: &Path,
        commands: &Commands,
    ) -> Result<(), Unready> {
        fs::create_dir_all(part).map_err(|e| format!("dibs: {}: {e}\n", part.display()))?;
        let layout = format!("{}:image", part.join("oci").display());
        tool(
            commands,
            "skopeo",
            &[
                "copy",
                "--quiet",
                &format!("docker://{}", image.pinned()),
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
        Account::add(&rootfs).map_err(|e| format!("dibs: the image's root filesystem: {e}\n"))?;
        Ok(fs::rename(&bundle, done).map_err(|e| format!("dibs: {}: {e}\n", done.display()))?)
    }

    /// The sandbox a command runs in: the image's root, this account's home and the machine's
    /// scratch at their own paths, so every cache dibs keeps is where the command expects it.
    fn sandbox(&self, rootfs: PathBuf) -> Result<Sandbox, Unready> {
        let sandbox = self.layout(rootfs);
        sandbox
            .mount_points()
            .map_err(|e| format!("dibs: the image's root filesystem: {e}\n"))?;
        Ok(sandbox)
    }

    fn layout(&self, rootfs: PathBuf) -> Sandbox {
        let mut binds = vec![self.home.to_path_buf()];
        // Scratch, or a part of it moved to another disk, may be a link, which must resolve
        // inside.
        let scratch = fs::canonicalize(self.scratch).unwrap_or_else(|_| self.scratch.into());
        if !scratch.starts_with(self.home) {
            binds.push(scratch.clone());
        }
        let moved: Vec<PathBuf> = fs::read_dir(&scratch)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_symlink()))
                    .filter_map(|e| fs::canonicalize(e.path()).ok())
                    .filter(|real| real.is_dir() && !binds.iter().any(|b| real.starts_with(b)))
                    .collect()
            })
            .unwrap_or_default();
        binds.extend(moved);
        // Sealed by both names, since either may be reachable through a bind.
        let mut sealed = vec![scratch.join(IMAGES)];
        if scratch != self.scratch {
            sealed.push(self.scratch.join(IMAGES));
        }
        // Jobs name scratch by the link, so its target is bound there too.
        let aliases = match scratch != self.scratch && !self.scratch.starts_with(self.home) {
            true => vec![(scratch, self.scratch.to_path_buf())],
            false => Vec::new(),
        };
        Sandbox {
            rootfs,
            binds,
            aliases,
            sealed,
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
            let path = within(rootfs, Path::new("etc"))?.join(file);
            if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(io::Error::other(format!("{} is a link", path.display())));
            }
            let image = fs::read_to_string(&path).unwrap_or_default();
            fs::write(&path, Account::replace(&image, &line))?;
        }
        Ok(())
    }

    /// The image's entries, less any with the account's name or id, which lookups by id would
    /// otherwise find first, then the account's.
    fn replace(image: &str, line: &str) -> String {
        let fields = |l: &str| {
            let mut f = l.split(':');
            (f.next().map(str::to_string), f.nth(1).map(str::to_string))
        };
        let (name, id) = fields(line);
        let mut text: String = image
            .lines()
            .filter(|l| {
                let (n, i) = fields(l);
                n != name && i != id
            })
            .flat_map(|l| [l, "\n"])
            .collect();
        text.push_str(line);
        text.push('\n');
        text
    }
}

/// How a command runs in an image: under bubblewrap, a child of the job like any command, so
/// the job's process group, its freeze and its descendants' CPU all still reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    rootfs: PathBuf,
    binds: Vec<PathBuf>,
    /// A bound directory again, at the path of a link to it: (directory, link).
    aliases: Vec<(PathBuf, PathBuf)>,
    /// Bound read-only over the binds: the unpacked images every build shares.
    sealed: Vec<PathBuf>,
}

impl Sandbox {
    /// The root is bound read-only, so what is bound over it needs its mount point made first.
    fn mount_points(&self) -> io::Result<()> {
        let links = self.aliases.iter().map(|(_, link)| link);
        let mut made: Vec<&Path> = Vec::new();
        for dir in self.binds.iter().chain(links).chain(&self.sealed) {
            // What an earlier bind covers comes from the host, so the image's is never seen.
            if !made.iter().any(|m| dir.starts_with(m)) {
                within(&self.rootfs, dir)?;
                made.push(dir);
            }
        }
        for file in NETWORK.iter().map(Path::new).filter(|f| f.exists()) {
            let dir = within(&self.rootfs, file.parent().unwrap_or(Path::new("/")))?;
            let point = dir.join(file.file_name().unwrap_or_default());
            if fs::symlink_metadata(&point).is_ok_and(|m| m.file_type().is_symlink()) {
                match fs::remove_file(&point) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
            // Made new, so never through a link; another job may have made it first.
            match File::options().write(true).create_new(true).open(&point) {
                Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }

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
        for file in NETWORK {
            if Path::new(file).exists() {
                args.extend(["--ro-bind".into(), file.into(), file.into()]);
            }
        }
        for dir in &self.binds {
            let dir = dir.display().to_string();
            args.extend(["--bind".into(), dir.clone(), dir]);
        }
        for (dir, link) in &self.aliases {
            args.extend([
                "--bind".into(),
                dir.display().to_string(),
                link.display().to_string(),
            ]);
        }
        for dir in self.sealed.iter().filter(|d| d.is_dir()) {
            let dir = dir.display().to_string();
            args.extend(["--ro-bind".into(), dir.clone(), dir]);
        }
        args.extend(["--".into(), "bash".into(), "-c".into(), command.into()]);
        args
    }
}

fn tool(commands: &Commands, name: &str, args: &[&str]) -> Result<(), Unready> {
    let mut command = Command::new(name);
    command.args(args);
    let output = commands.output(command).map_err(|e| match e.kind() {
        io::ErrorKind::TimedOut => {
            overran(format!("dibs: the job's cap ran out while {name} prepared its image\n"))
        }
        io::ErrorKind::NotFound => format!(
            "dibs: building in an image needs skopeo, umoci and bwrap on this machine, and {name} is not installed\n"
        )
        .into(),
        _ => format!("dibs: {name} could not start: {e}\n").into(),
    })?;
    match output.status.success() {
        true => Ok(()),
        false => Err(format!(
            "dibs: {name} {} failed ({}):\n{}",
            args.first().unwrap_or(&""),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into()),
    }
}

/// `path` inside the image's root, its missing directories made. A link the image holds is
/// refused rather than followed, since it would be followed on the host.
fn within(rootfs: &Path, path: &Path) -> io::Result<PathBuf> {
    let mut at = rootfs.to_path_buf();
    for part in path.components() {
        let Component::Normal(part) = part else {
            continue;
        };
        at.push(part);
        match fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(io::Error::other(format!(
                    "{} is a link in the image, which dibs cannot bind over",
                    at.display()
                )));
            }
            Ok(_) => {}
            // Another job preparing the same image may make it first.
            Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(&at) {
                Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                _ => {}
            },
            Err(e) => return Err(e),
        }
    }
    Ok(at)
}

/// Removes an unpacked tree, whose directories an image may have left read-only, within the
/// job's cap; one left behind is removed by the next unpack of its image.
fn remove(commands: &Commands, path: &Path) -> Result<(), Unready> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    let path = path.display().to_string();
    tool(commands, "chmod", &["-R", "u+w", &path])?;
    tool(commands, "rm", &["-rf", &path])
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
        assert!(Image::parse(&format!("@sha256:{DIGEST}")).is_err());
        assert!(Image::parse(&format!(":noble@sha256:{DIGEST}")).is_err());
        assert!(Image::parse("x@sha256:0123").is_err());
        assert!(Image::parse(&format!("x@sha256:{}", DIGEST.to_uppercase())).is_err());
    }

    #[test]
    fn a_registry_is_asked_for_the_digest_without_the_tag() {
        let digest = "0".repeat(super::DIGEST);
        for (named, asked) in [
            ("docker.io/library/b:noble", "docker.io/library/b"),
            ("localhost:5000/b:noble", "localhost:5000/b"),
            ("localhost:5000/b", "localhost:5000/b"),
        ] {
            let image = Image::parse(&format!("{named}@sha256:{digest}")).unwrap();
            assert_eq!(image.pinned(), format!("{asked}@sha256:{digest}"));
        }
    }

    #[test]
    fn a_target_says_which_image_built_it() {
        assert_eq!(
            Image::tag_of("burn-local-k1-img0123456789ab"),
            Some("-img0123456789ab")
        );
        assert_eq!(Image::tag_of("burn-local-k1"), None);
        assert_eq!(Image::tag_of("burn-imgfoo"), None);
        assert_eq!(
            Image::untagged("burn-local-k1-img0123456789ab"),
            "burn-local-k1"
        );
        assert_eq!(Image::untagged("burn-imgfoo"), "burn-imgfoo");
    }

    #[test]
    fn a_path_an_earlier_bind_covers_needs_no_mount_point_in_the_image() {
        let root = std::env::temp_dir().join(format!("dibs-image-under.{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let scratch = home.join(".cache/dibs");
        fs::create_dir_all(scratch.join(IMAGES)).unwrap();
        let rootfs = root.join("rootfs");
        let cache = rootfs.join(home.join(".cache").strip_prefix("/").unwrap());
        fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/nowhere", &cache).unwrap();
        let images = Images {
            scratch: &scratch,
            home: &home,
        };
        assert!(images.sandbox(rootfs).is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_account_replaces_an_image_entry_with_its_id() {
        let image = "root:x:0:0::/root:/bin/bash\nubuntu:x:1000:1000::/home/ubuntu:/bin/bash\n";
        let ours = "dibs:x:1000:1000::/home/dibs:/bin/bash";
        assert_eq!(
            Account::replace(image, ours),
            format!("root:x:0:0::/root:/bin/bash\n{ours}\n")
        );
    }

    #[test]
    fn a_sandbox_binds_home_and_scratch_at_their_own_paths() {
        let images = Images {
            scratch: Path::new("/data/dibs"),
            home: Path::new("/home/dibs"),
        };
        let args = images
            .layout(PathBuf::from("/data/dibs/images/d/rootfs"))
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
        let args = inside.layout(PathBuf::from("/r")).arguments("true");
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
        let rootfs = root.join("rootfs");
        fs::create_dir_all(&rootfs).unwrap();
        let joined = images
            .sandbox(rootfs.clone())
            .unwrap()
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
        assert!(rootfs.join(real.trim_start_matches('/')).is_dir());
        let linked = root.join("linked");
        std::os::unix::fs::symlink(&scratch, &linked).unwrap();
        let through = Images {
            scratch: &linked,
            home: &home,
        }
        .sandbox(rootfs.clone())
        .unwrap()
        .arguments("true")
        .join(" ");
        let (at, real_scratch) = (
            linked.display().to_string(),
            fs::canonicalize(&scratch).unwrap().display().to_string(),
        );
        assert!(
            through.contains(&format!("--bind {real_scratch} {at} ")),
            "jobs name scratch by its link: {through}"
        );
        assert!(
            through.contains(&format!("--ro-bind {at}/images {at}/images")),
            "{through}"
        );
        let real_images = format!("{real_scratch}/images");
        assert!(
            through.contains(&format!("--ro-bind {real_images} {real_images}")),
            "sealed by its real name too: {through}"
        );
        if Path::new("/etc/hosts").exists() {
            assert!(rootfs.join("etc/hosts").is_file());
        }
        fs::create_dir_all(root.join("elsewhere")).unwrap();
        let _ = fs::remove_dir_all(rootfs.join("etc"));
        std::os::unix::fs::symlink(root.join("elsewhere"), rootfs.join("etc")).unwrap();
        assert!(
            images.sandbox(rootfs.clone()).is_err(),
            "a link in the image is not followed"
        );
        assert_eq!(fs::read_dir(root.join("elsewhere")).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&root);
    }
}
