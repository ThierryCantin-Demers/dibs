use crate::{
    machine::short_hostname,
    platform::{Host, Platform as _},
    settings::home,
    stop::Signals,
};
use dibs_format::fleet::Facts;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Where tools are looked for besides `PATH`: a login shell over ssh reads no profile, and
/// these are where rustup, Homebrew and CUDA put theirs.
const ALSO: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/local/cuda/bin"];
/// Where ldconfig is, off the PATH of an account that is not root.
const LDCONFIG: [&str; 2] = ["/sbin", "/usr/sbin"];

/// The directories a tool is looked for in.
pub struct Tools {
    dirs: Vec<PathBuf>,
}

impl Tools {
    /// The platform's tools first, as a job has them, so the rsync the check finds is the one a
    /// transfer runs.
    pub fn here() -> Tools {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = Host::tools_first();
        dirs.extend(std::env::split_paths(&path));
        dirs.push(home().join(".cargo/bin"));
        dirs.extend(ALSO.iter().map(PathBuf::from));
        Tools { dirs }
    }

    pub fn find(&self, program: &str) -> Option<PathBuf> {
        self.dirs
            .iter()
            .map(|dir| dir.join(program))
            .find(|p| is_executable(p))
    }

    /// What a tool printed on stdout, None when it is not there, could not run or printed
    /// nothing.
    pub fn output(&self, program: &str, args: &[&str]) -> Option<String> {
        let mut command = Command::new(self.find(program)?);
        command
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        Signals::unblocked(&mut command);
        let out = command.output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        (!text.trim().is_empty()).then_some(text)
    }

    fn first_line(&self, program: &str, args: &[&str]) -> Option<String> {
        self.output(program, args)?
            .lines()
            .next()
            .map(str::to_string)
    }

    /// Whether a tool ran and succeeded.
    fn succeeds(&self, program: &str, args: &[&str]) -> bool {
        self.find(program).is_some_and(|p| {
            let mut command = Command::new(p);
            command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            Signals::unblocked(&mut command);
            command.status().is_ok_and(|s| s.success())
        })
    }

    /// What the machine has, read once.
    pub fn facts(&self) -> Facts {
        let uname = Uname::read();
        Facts {
            user: std::env::var("USER").unwrap_or_default(),
            host: short_hostname(None),
            os: uname.os,
            arch: uname.arch,
            bash: self
                .first_line("bash", &["--version"])
                .and_then(|l| bash_version(&l)),
            rsync: self
                .first_line("rsync", &["--version"])
                .and_then(|l| rsync_version(&l)),
            git: self.find("git").is_some(),
            cargo: self.find("cargo").is_some(),
            rustup: self.find("rustup").is_some(),
            toolchains: self
                .output("rustup", &["toolchain", "list"])
                .map(|text| {
                    text.lines()
                        .filter_map(|l| l.split_whitespace().next())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            nvidia: self
                .first_line(
                    "nvidia-smi",
                    &["--query-gpu=driver_version", "--format=csv,noheader"],
                )
                .map(|l| l.trim().to_string()),
            nvcc: self
                .output("nvcc", &["--version"])
                .and_then(|text| nvcc_release(&text)),
            vulkan: Tools::vulkan(),
            nopasswd: self.succeeds("sudo", &["-n", "-l"]),
            groups: self
                .output("id", &["-Gn"])
                .map(|g| g.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
            keys: self.keys(),
            tailscale_ssh: self
                .output("tailscale", &["debug", "prefs"])
                .is_some_and(|prefs| prefs.contains("\"RunSSH\": true")),
            repos: self.repos(),
        }
    }

    /// Whether the Vulkan loader is installed where the dynamic linker finds it.
    fn vulkan() -> bool {
        Tools {
            dirs: LDCONFIG.map(PathBuf::from).to_vec(),
        }
        .output("ldconfig", &["-p"])
        .is_some_and(|libs| libs.contains("libvulkan.so.1"))
    }

    fn keys(&self) -> Vec<String> {
        let file = home().join(".ssh/authorized_keys");
        let Some(file) = file.to_str() else {
            return Vec::new();
        };
        self.output("ssh-keygen", &["-lf", file])
            .map(|listed| {
                let keys: BTreeSet<String> = listed
                    .lines()
                    .filter_map(|l| l.split_whitespace().nth(1))
                    .map(str::to_string)
                    .collect();
                keys.into_iter().collect()
            })
            .unwrap_or_default()
    }

    /// The clones under `~/prog`, each with its origin, or none when it has no origin.
    fn repos(&self) -> BTreeMap<String, String> {
        fs::read_dir(home().join("prog"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join(".git").exists())
            .map(|e| {
                let dir = e.path();
                let origin = dir
                    .to_str()
                    .and_then(|d| self.output("git", &["-C", d, "remote", "get-url", "origin"]))
                    .map(|url| url.trim().to_string())
                    .unwrap_or_default();
                (e.file_name().to_string_lossy().into_owned(), origin)
            })
            .collect()
    }
}

/// `uname`'s system and machine names.
struct Uname {
    os: String,
    arch: String,
}

impl Uname {
    fn read() -> Uname {
        // SAFETY: utsname is plain data, which uname fills with NUL-terminated strings.
        let mut names: libc::utsname = unsafe { std::mem::zeroed() };
        if unsafe { libc::uname(&mut names) } != 0 {
            return Uname {
                os: String::new(),
                arch: String::new(),
            };
        }
        let text = |field: &[libc::c_char]| {
            // SAFETY: uname wrote a NUL-terminated string into the field.
            unsafe { CStr::from_ptr(field.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        Uname {
            os: text(&names.sysname),
            arch: text(&names.machine),
        }
    }
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| {
        m.is_file() && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
    })
}

/// `5.2.26` of `GNU bash, version 5.2.26(1)-release (...)`.
fn bash_version(line: &str) -> Option<String> {
    let after = line.split_once("version ")?.1;
    let version: String = after
        .chars()
        .take_while(|c| *c != '(' && *c != ' ')
        .collect();
    (!version.is_empty()).then_some(version)
}

/// `3.2.7` of `rsync  version 3.2.7  protocol version 31`, and nothing older than 3, which
/// takes too few of the options a sent tree needs.
fn rsync_version(line: &str) -> Option<String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    (words.first() == Some(&"rsync"))
        .then(|| words.get(2).map(|v| v.to_string()))?
        .filter(|v| v.starts_with(|c: char| ('3'..='9').contains(&c)))
}

/// `12.4` of nvcc's `Cuda compilation tools, release 12.4, V12.4.131`.
fn nvcc_release(text: &str) -> Option<String> {
    let after = text.split_once("release ")?.1;
    let release: String = after.chars().take_while(|c| *c != ',').collect();
    (!release.is_empty()).then_some(release)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_says_its_version_before_its_build() {
        assert_eq!(
            bash_version("GNU bash, version 5.2.26(1)-release (x86_64-redhat-linux-gnu)")
                .as_deref(),
            Some("5.2.26")
        );
        assert_eq!(bash_version("no such thing"), None);
    }

    #[test]
    fn only_rsync_3_or_later_counts() {
        assert_eq!(
            rsync_version("rsync  version 3.2.7  protocol version 31").as_deref(),
            Some("3.2.7")
        );
        assert_eq!(rsync_version("openrsync: protocol version 29"), None);
        assert_eq!(
            rsync_version("rsync  version 2.6.9  protocol version 29"),
            None
        );
    }

    #[test]
    fn nvcc_says_its_release() {
        let said = "nvcc: NVIDIA (R) Cuda compiler driver\nCuda compilation tools, release 12.4, V12.4.131\n";
        assert_eq!(nvcc_release(said).as_deref(), Some("12.4"));
        assert_eq!(nvcc_release("nothing"), None);
    }
}
