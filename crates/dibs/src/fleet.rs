//! What each machine should have, read against what it has.
//!
//! The inventory says how to reach a machine and `--check` what is in it, but nothing said what
//! it should have, so a machine set up by hand drifted from the others unseen until a job failed
//! on it.

use crate::{
    call::{LockedCall, Origin, Output, RecipeJob},
    caller::Caller,
    cli::{Call, Command as ShellCommand, Mode, Run},
    inventory::{Inventory, InventoryError},
    machine::Stream,
    paths::Paths,
};
use dibs_format::{Exit, Label, MachineName};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::Duration,
};

/// Written for POSIX sh, since it has to run before a machine has what dibs needs, and few enough
/// lines that a job's digest of its output keeps all of them.
pub const PROBE: &str = r#"PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/local/cuda/bin:$PATH"
p() { printf 'DIBS-PROBE %s\n' "$*"; }
have() { command -v "$1" 2>/dev/null; }
p sys "user=$(id -un)" "host=$(hostname -s 2>/dev/null || hostname)" "os=$(uname -s)" "arch=$(uname -m)"
best=$(for b in "$(have bash)" /opt/homebrew/bin/bash /usr/local/bin/bash /bin/bash; do
    [ -n "$b" ] && [ -x "$b" ] && "$b" -c 'echo "${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}"' 2>/dev/null
done | sort -t. -k1,1n -k2,2n | tail -n 1)
p bash "version=$best"
p tools "flock=$(have flock)" "timeout=$(have timeout)" "gtimeout=$(have gtimeout)" "rsync=$(rsync --version 2>/dev/null | awk 'NR == 1 && $1 == "rsync" {print $3}')" "git=$(have git)"
p rust "rustup=$(have rustup)" "toolchains=$(rustup toolchain list 2>/dev/null | awk '{print $1}' | paste -sd, -)"
vk=no
{ /sbin/ldconfig -p || /usr/sbin/ldconfig -p; } 2>/dev/null | grep -q 'libvulkan\.so\.1' && vk=yes
p gpu "nvidia=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -n 1)" "nvcc=$(nvcc --version 2>/dev/null | sed -n 's/.*release \([0-9.]*\),.*/\1/p')" "vulkan=$vk"
np=no
sudo -n -l >/dev/null 2>&1 && np=yes
p account "nopasswd=$np" "groups=$(id -Gn | tr ' ' ,)"
p keys $(ssh-keygen -lf "$HOME/.ssh/authorized_keys" 2>/dev/null | awk '{print $2}')
ts=no
tailscale debug prefs 2>/dev/null | grep -q '"RunSSH": true' && ts=yes
p login "tailscale_ssh=$ts"
r=
for d in "$HOME"/prog/*/; do
    [ -e "$d.git" ] || continue
    r="$r $(basename "$d")=$(git -C "$d" remote get-url origin 2>/dev/null)"
done
p repos $r
p disk "free_kb=$(df -Pk "$HOME" | awk 'NR==2{print $4}')"
"#;

/// Groups whose members are root in all but name.
const PRIVILEGED_GROUPS: [&str; 4] = ["sudo", "admin", "wheel", "docker"];
const BASH_NEEDED: (u32, u32) = (5, 1);
/// Long enough to go around a quick shared job, short enough not to sit out a benchmark.
const PROBE_WAIT_SECONDS: u64 = 30;
const PROBE_LABEL: &str = "machines-probe";
const SSH_UNREACHABLE: i32 = 255;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fleet {
    #[serde(default)]
    person: BTreeMap<String, Person>,
    #[serde(default)]
    machine: BTreeMap<String, Machine>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Person {
    keys: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Machine {
    provisioned: Provisioned,
    /// How to reach a machine not yet in the pool. One in the pool is only ever reached through
    /// dibs, under its lock.
    ssh: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    people: Vec<String>,
    #[serde(default)]
    login: Login,
    #[serde(default)]
    profiles: Vec<Profile>,
    /// Every repo with recipes when absent.
    repos: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct Provisioned {
    by: Provisioner,
    source: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Provisioner {
    Ansible,
    Hand,
}

/// How a person gets in, which decides where their access is written down.
#[derive(Deserialize, Clone, Copy, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Login {
    /// A key in the account's `authorized_keys`.
    #[default]
    Keys,
    /// Tailscale SSH, where the tailnet's policy says who may, and no key is involved.
    Tailscale,
}

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
enum Profile {
    Dibs,
    Rust,
    Cuda,
    Vulkan,
    Metal,
    Unprivileged,
}

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
enum Area {
    Paths,
    Login,
    Repos,
    Dibs,
    Rust,
    Cuda,
    Vulkan,
    Metal,
    Account,
}

impl Area {
    fn name(self) -> &'static str {
        match self {
            Area::Paths => "paths",
            Area::Login => "login",
            Area::Repos => "repos",
            Area::Dibs => "dibs",
            Area::Rust => "rust",
            Area::Cuda => "cuda",
            Area::Vulkan => "vulkan",
            Area::Metal => "metal",
            Area::Account => "account",
        }
    }
}

/// Where one person stands on one machine.
#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
enum Standing {
    /// Listed, with a key in `authorized_keys`.
    Key,
    /// Listed, with no key there.
    Missing,
    /// Not listed, with a key there anyway.
    Unlisted,
    /// Listed, on a machine the tailnet's policy decides.
    Tailnet,
}

/// Who can get in, as far as the machine can say.
#[derive(Serialize, Default)]
struct Access {
    people: BTreeMap<String, Standing>,
    /// Fingerprints of keys that belong to nobody in fleet.toml.
    strangers: Vec<String>,
}

#[derive(Serialize)]
struct PathCheck {
    name: String,
    problem: Option<String>,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Via {
    Dibs,
    Ssh,
}

#[derive(Serialize, Debug)]
struct Finding {
    area: Area,
    ok: bool,
    detail: String,
}

impl Finding {
    fn new(area: Area, missing: Vec<String>, present: String) -> Finding {
        match missing.is_empty() {
            true => Finding {
                area,
                ok: true,
                detail: present,
            },
            false => Finding {
                area,
                ok: false,
                detail: missing.join(", "),
            },
        }
    }
}

#[derive(Serialize)]
struct Report {
    machine: String,
    provisioned: Provisioned,
    via: Via,
    /// Why nothing was read from the machine, when nothing was.
    unprobed: Option<String>,
    paths: Vec<PathCheck>,
    access: Access,
    findings: Vec<Finding>,
}

#[derive(Serialize)]
struct Overview<'a> {
    people: Vec<&'a String>,
    machines: &'a [Report],
}

impl Report {
    fn as_expected(&self) -> bool {
        self.unprobed.is_none() && self.findings.iter().all(|f| f.ok)
    }
}

/// What the probe printed, by section.
#[derive(Default)]
struct Observed {
    fields: BTreeMap<String, String>,
    keys: BTreeSet<String>,
    repos: BTreeMap<String, String>,
}

impl Observed {
    fn parse(text: &str) -> Observed {
        let mut o = Observed::default();
        for rest in text.lines().filter_map(|l| l.strip_prefix("DIBS-PROBE ")) {
            let mut words = rest.split_whitespace();
            let Some(section) = words.next() else {
                continue;
            };
            for w in words {
                match (section, w.split_once('=')) {
                    ("keys", _) => {
                        o.keys.insert(w.to_string());
                    }
                    ("repos", Some((name, url))) => {
                        o.repos.insert(name.to_string(), url.to_string());
                    }
                    (_, Some((k, v))) => {
                        o.fields.insert(format!("{section}.{k}"), v.to_string());
                    }
                    _ => {}
                }
            }
        }
        o
    }

    fn get(&self, key: &str) -> &str {
        self.fields.get(key).map_or("", String::as_str)
    }

    fn has(&self, key: &str) -> bool {
        !self.get(key).is_empty()
    }

    fn bash(&self) -> Option<(u32, u32)> {
        let (major, minor) = self.get("bash.version").split_once('.')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    }

    fn toolchains(&self) -> Vec<&str> {
        self.get("rust.toolchains")
            .split(',')
            .filter(|t| !t.is_empty())
            .collect()
    }
}

/// What a machine is checked against, beyond its own entry.
struct Context {
    /// Each key's fingerprint, to whom it belongs.
    owners: BTreeMap<String, String>,
    /// The toolchain a repo pins; a repo absent here builds on stable.
    pins: BTreeMap<String, String>,
    recipe_repos: Vec<String>,
}

impl Machine {
    fn repos<'a>(&'a self, cx: &'a Context) -> &'a [String] {
        self.repos.as_deref().unwrap_or(&cx.recipe_repos)
    }

    fn check(&self, o: &Observed, cx: &Context) -> Vec<Finding> {
        let login = match self.login {
            Login::Keys => self.keys(&self.access(o, cx)),
            Login::Tailscale => self.tailscale(o, cx),
        };
        let mut out = vec![login, self.repo_clones(o, cx)];
        out.extend(self.profiles.iter().map(|p| p.check(o, self, cx)));
        out
    }

    fn access(&self, o: &Observed, cx: &Context) -> Access {
        let holds = |p: &str| {
            o.keys
                .iter()
                .any(|fp| cx.owners.get(fp).is_some_and(|who| who == p))
        };
        let mut people: BTreeMap<String, Standing> = self
            .people
            .iter()
            .map(|p| {
                let standing = match (self.login, holds(p)) {
                    (Login::Tailscale, _) => Standing::Tailnet,
                    (Login::Keys, true) => Standing::Key,
                    (Login::Keys, false) => Standing::Missing,
                };
                (p.clone(), standing)
            })
            .collect();
        let mut strangers = Vec::new();
        for fp in &o.keys {
            match cx.owners.get(fp) {
                None => strangers.push(fp.clone()),
                Some(who) if !self.people.contains(who) => {
                    people.insert(who.clone(), Standing::Unlisted);
                }
                Some(_) => {}
            }
        }
        Access { people, strangers }
    }

    fn keys(&self, a: &Access) -> Finding {
        let mut missing: Vec<String> = a
            .people
            .iter()
            .filter_map(|(p, standing)| match standing {
                Standing::Missing => Some(format!("no key of {p}")),
                Standing::Unlisted => Some(format!("{p}'s key, and {p} is not listed here")),
                Standing::Key | Standing::Tailnet => None,
            })
            .collect();
        missing.extend(
            a.strangers
                .iter()
                .map(|fp| format!("a key of nobody listed: {fp}")),
        );
        Finding::new(
            Area::Login,
            missing,
            format!("keys of {}", self.people.join(", ")),
        )
    }

    fn tailscale(&self, o: &Observed, cx: &Context) -> Finding {
        let mut missing = Vec::new();
        if o.get("login.tailscale_ssh") != "yes" {
            missing.push("Tailscale SSH is off".to_string());
        }
        if !o.keys.is_empty() {
            let whose: Vec<&str> = o
                .keys
                .iter()
                .map(|fp| cx.owners.get(fp).map_or(fp.as_str(), String::as_str))
                .collect();
            missing.push(format!(
                "a second way in, keys in authorized_keys: {}",
                whose.join(", ")
            ));
        }
        Finding::new(
            Area::Login,
            missing,
            "by Tailscale SSH, whoever the tailnet's policy lets in".into(),
        )
    }

    fn repo_clones(&self, o: &Observed, cx: &Context) -> Finding {
        let wanted = self.repos(cx);
        let missing = wanted
            .iter()
            .filter(|r| !o.repos.contains_key(*r))
            .map(|r| format!("no clone of {r}"))
            .collect();
        Finding::new(Area::Repos, missing, wanted.join(", "))
    }
}

impl Profile {
    fn check(self, o: &Observed, m: &Machine, cx: &Context) -> Finding {
        match self {
            Profile::Dibs => {
                let mut missing: Vec<String> = ["flock", "rsync", "git"]
                    .iter()
                    .filter(|t| !o.has(&format!("tools.{t}")))
                    .map(|t| t.to_string())
                    .collect();
                if !o.has("tools.timeout") && !o.has("tools.gtimeout") {
                    missing.push("GNU timeout".into());
                }
                match o.bash() {
                    Some(v) if v >= BASH_NEEDED => {}
                    Some((major, minor)) => missing.push(format!(
                        "bash {major}.{minor}, needs {}.{}",
                        BASH_NEEDED.0, BASH_NEEDED.1
                    )),
                    None => missing.push("bash".into()),
                }
                Finding::new(
                    Area::Dibs,
                    missing,
                    format!("bash {}", o.get("bash.version")),
                )
            }
            Profile::Rust => {
                if !o.has("rust.rustup") {
                    return Finding::new(Area::Rust, vec!["no rustup".into()], String::new());
                }
                let have = o.toolchains();
                let needed: BTreeSet<&str> = std::iter::once("stable")
                    .chain(
                        m.repos(cx)
                            .iter()
                            .filter_map(|r| cx.pins.get(r).map(String::as_str)),
                    )
                    .collect();
                let installed = |c: &str| {
                    have.iter()
                        .any(|t| *t == c || t.starts_with(&format!("{c}-")))
                };
                let missing = needed
                    .iter()
                    .filter(|c| !installed(c))
                    .map(|c| format!("no {c} toolchain"))
                    .collect();
                Finding::new(
                    Area::Rust,
                    missing,
                    needed.into_iter().collect::<Vec<_>>().join(", "),
                )
            }
            Profile::Cuda => {
                let mut missing = Vec::new();
                if !o.has("gpu.nvidia") {
                    missing.push("no NVIDIA driver".into());
                }
                if !o.has("gpu.nvcc") {
                    missing.push("no nvcc".into());
                }
                Finding::new(
                    Area::Cuda,
                    missing,
                    format!("driver {}, nvcc {}", o.get("gpu.nvidia"), o.get("gpu.nvcc")),
                )
            }
            Profile::Vulkan => {
                let missing = (o.get("gpu.vulkan") != "yes")
                    .then(|| "no Vulkan loader".to_string())
                    .into_iter()
                    .collect();
                Finding::new(Area::Vulkan, missing, "loader present".into())
            }
            Profile::Metal => {
                let missing = (o.get("sys.os") != "Darwin")
                    .then(|| format!("{} has no Metal", o.get("sys.os")))
                    .into_iter()
                    .collect();
                Finding::new(Area::Metal, missing, "macOS".into())
            }
            Profile::Unprivileged => {
                let mut missing = Vec::new();
                if o.get("account.nopasswd") == "yes" {
                    missing.push("sudo without a password".to_string());
                }
                let groups: Vec<&str> = o
                    .get("account.groups")
                    .split(',')
                    .filter(|g| PRIVILEGED_GROUPS.contains(g))
                    .collect();
                if !groups.is_empty() {
                    missing.push(format!("in {}", groups.join(", ")));
                }
                Finding::new(
                    Area::Account,
                    missing,
                    format!("{}, no root", o.get("sys.user")),
                )
            }
        }
    }
}

/// `DIBS_FLEET`, or beside the inventory.
pub fn path() -> Result<PathBuf, String> {
    Paths::from_env()
        .fleet()
        .ok_or_else(|| "no HOME to find fleet.toml under".into())
}

fn load(path: &Path) -> Result<Fleet, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "{}: {e}\n  It says what each machine should have: people and their keys, and per machine how it was set\n  \
             up, the names it is reached by, who may log in and the profiles it needs. See dibs-design/machines.md.",
            path.display()
        )
    })?;
    let fleet: Fleet = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for (name, m) in &fleet.machine {
        if let Some(p) = m.people.iter().find(|p| !fleet.person.contains_key(*p)) {
            return Err(format!(
                "{}: machine {name} lists {p}, who has no [person.{p}]",
                path.display()
            ));
        }
    }
    Ok(fleet)
}

/// Each key file's fingerprint, read the way the machine's side reads its `authorized_keys`.
fn owners(fleet: &Fleet, dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (who, person) in &fleet.person {
        for key in &person.keys {
            let file = dir.join(key);
            let listed = Command::new("ssh-keygen")
                .arg("-lf")
                .arg(&file)
                .output()
                .map_err(|e| format!("ssh-keygen: {e}"))?;
            if !listed.status.success() {
                return Err(format!("{}: not a public key ({who})", file.display()));
            }
            for fp in String::from_utf8_lossy(&listed.stdout)
                .lines()
                .filter_map(|l| l.split_whitespace().nth(1))
            {
                out.insert(fp.to_string(), who.clone());
            }
        }
    }
    Ok(out)
}

fn pin(checkout: &Path) -> Option<String> {
    if let Ok(text) = std::fs::read_to_string(checkout.join("rust-toolchain.toml")) {
        let v: toml::Value = toml::from_str(&text).ok()?;
        return v
            .get("toolchain")?
            .get("channel")?
            .as_str()
            .map(str::to_string);
    }
    let legacy = std::fs::read_to_string(checkout.join("rust-toolchain")).ok()?;
    legacy
        .lines()
        .next()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

/// How a machine was probed, and what it said or why it said nothing.
struct Probed {
    via: Via,
    observed: Result<Observed, String>,
}

/// What a probe printed, and its exit.
struct Heard {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn probe(name: &str, m: &Machine, pool: &BTreeSet<String>) -> Probed {
    let (via, heard) = match (pool.contains(name), &m.ssh) {
        (true, _) => (Via::Dibs, Ok(over_dibs(name))),
        (false, Some(target)) => (Via::Ssh, over_ssh(target)),
        (false, None) => {
            return Probed {
                via: Via::Ssh,
                observed: Err("not in the pool, and no ssh to reach it by: give it one, or record it with dibs --check".into()),
            };
        }
    };
    let heard = match heard {
        Ok(heard) => heard,
        Err(e) => {
            return Probed {
                via,
                observed: Err(e.to_string()),
            };
        }
    };
    let said = || {
        String::from_utf8_lossy(&heard.stderr)
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let busy = i32::from(Exit::Busy.code());
    let unreachable = i32::from(Exit::Unreachable.code());
    let observed = match heard.status {
        Some(0) => Ok(Observed::parse(&String::from_utf8_lossy(&heard.stdout))),
        Some(code) if code == busy => {
            Err("busy: a benchmark holds it, so it was not probed".into())
        }
        Some(code) if code == unreachable || code == SSH_UNREACHABLE => {
            Err(format!("unreachable: {}", said()))
        }
        code => Err(format!(
            "the probe failed (exit {}): {}",
            code.unwrap_or(-1),
            said()
        )),
    };
    Probed { via, observed }
}

/// The probe as a shared job on a machine in the pool, under its lock.
fn over_dibs(name: &str) -> Heard {
    let run = Run {
        command: ShellCommand(vec![PROBE.to_string()]),
        ..Run::default()
    };
    let call = Call {
        mode: Mode::Run(run.clone()),
        on: Some(MachineName::new(name)),
        wait: Some(PROBE_WAIT_SECONDS),
        label: Some(Label::new(PROBE_LABEL)),
        stream: true,
        ..Call::default()
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let job = RecipeJob::default();
    let exit = LockedCall::run_of(&run)
        .made_by(Origin::Recipe(&job))
        .run_into(
            &call,
            &Caller::from_env(),
            &mut Output::Lines(&mut |stream, line| match stream {
                Stream::Out => stdout.extend_from_slice(line),
                Stream::Err => stderr.extend_from_slice(line),
            }),
        );
    let status = exit.unwrap_or_else(|e| {
        stderr.extend_from_slice(e.to_string().as_bytes());
        e.exit()
    });
    Heard {
        status: Some(status),
        stdout,
        stderr,
    }
}

fn over_ssh(target: &str) -> std::io::Result<Heard> {
    let mut child = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            target,
            "sh -s",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(PROBE.as_bytes())?;
    let out = child.wait_with_output()?;
    Ok(Heard {
        status: out.status.code(),
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

/// From here: the name resolves, and something answers on the ssh port.
fn reach(name: &str) -> Result<(), String> {
    let addrs: Vec<_> = (name, 22)
        .to_socket_addrs()
        .map_err(|_| format!("{name} does not resolve here"))?
        .collect();
    match addrs
        .iter()
        .any(|a| TcpStream::connect_timeout(a, Duration::from_secs(3)).is_ok())
    {
        true => Ok(()),
        false => Err(format!("{name} resolves, and nothing answers on port 22")),
    }
}

fn reach_all(paths: &[String]) -> Vec<PathCheck> {
    std::thread::scope(|s| {
        let tried: Vec<_> = paths
            .iter()
            .map(|p| (p, s.spawn(move || reach(p))))
            .collect();
        tried
            .into_iter()
            .map(|(p, t)| PathCheck {
                name: p.clone(),
                problem: t.join().expect("a reach does not panic").err(),
            })
            .collect()
    })
}

fn report(name: &str, m: &Machine, cx: &Context, pool: &BTreeSet<String>) -> Report {
    let Probed { via, observed } = probe(name, m, pool);
    let paths = reach_all(&m.paths);
    let problems = paths.iter().filter_map(|p| p.problem.clone()).collect();
    let mut findings = vec![Finding::new(Area::Paths, problems, m.paths.join(", "))];
    let (unprobed, access) = match observed {
        Ok(o) => {
            findings.extend(m.check(&o, cx));
            (None, m.access(&o, cx))
        }
        Err(e) => (Some(e), Access::default()),
    };
    Report {
        machine: name.to_string(),
        provisioned: m.provisioned.clone(),
        via,
        unprobed,
        paths,
        access,
        findings,
    }
}

fn render(reports: &[Report]) -> String {
    let mut s = String::new();
    for r in reports {
        let by = match r.provisioned.by {
            Provisioner::Ansible => "ansible",
            Provisioner::Hand => "hand",
        };
        let source = r
            .provisioned
            .source
            .as_ref()
            .map(|x| format!(" ({x})"))
            .unwrap_or_default();
        let via = match r.via {
            Via::Dibs => "through dibs",
            Via::Ssh => "over ssh",
        };
        s += &format!("{}  set up by {by}{source}, probed {via}\n", r.machine);
        for f in &r.findings {
            s += &format!(
                "  {}  {:<8} {}\n",
                if f.ok { "ok" } else { "NO" },
                f.area.name(),
                f.detail
            );
        }
        if let Some(why) = &r.unprobed {
            s += &format!("  --  not probed: {why}\n");
        }
    }
    let good = reports.iter().filter(|r| r.as_expected()).count();
    let unprobed = reports.iter().filter(|r| r.unprobed.is_some()).count();
    s += &format!(
        "\n{} machine(s): {good} as expected, {} with something missing, {unprobed} not probed\n",
        reports.len(),
        reports.len() - good - unprobed
    );
    s
}

/// `dibs machines [<machine>]`: every machine in fleet.toml, or the one named, probed at once.
pub fn command(
    json: bool,
    only: Option<&str>,
    root: &Path,
    recipe_repos: Vec<String>,
    pool: &BTreeSet<String>,
) -> Result<ExitCode, String> {
    let path = path()?;
    let mut fleet = load(&path)?;
    if let Some(name) = only {
        if !fleet.machine.contains_key(name) {
            let have: Vec<&str> = fleet.machine.keys().map(String::as_str).collect();
            return Err(format!(
                "no machine {name} in {}; it has: {}",
                path.display(),
                have.join(", ")
            ));
        }
        fleet.machine.retain(|n, _| n == name);
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let wanted: BTreeSet<&String> = fleet
        .machine
        .values()
        .flat_map(|m| m.repos.iter().flatten())
        .chain(&recipe_repos)
        .collect();
    let pins = wanted
        .into_iter()
        .filter_map(|r| pin(&root.join(r)).map(|c| (r.clone(), c)))
        .collect();
    let cx = Context {
        owners: owners(&fleet, dir)?,
        pins,
        recipe_repos,
    };
    let reports: Vec<Report> = std::thread::scope(|s| {
        let running: Vec<_> = fleet
            .machine
            .iter()
            .map(|(name, m)| s.spawn(|| report(name, m, &cx, pool)))
            .collect();
        running
            .into_iter()
            .map(|r| r.join().expect("a probe does not panic"))
            .collect()
    });
    match json {
        true => {
            let overview = Overview {
                people: fleet.person.keys().collect(),
                machines: &reports,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&overview).map_err(|e| e.to_string())?
            )
        }
        false => print!("{}", render(&reports)),
    }
    Ok(match reports.iter().all(Report::as_expected) {
        true => ExitCode::SUCCESS,
        false => ExitCode::from(Exit::Failed.code()),
    })
}

pub fn inventory_path() -> Option<PathBuf> {
    Paths::from_env().inventory()
}

/// The machines the inventory names, which are reached only through dibs.
pub fn pool() -> Result<BTreeSet<String>, InventoryError> {
    Ok(inventory()?
        .map(|i| i.names().map(MachineName::to_string).collect())
        .unwrap_or_default())
}

/// The inventory, when there is a file; one that does not read is an error.
pub fn inventory() -> Result<Option<Inventory>, InventoryError> {
    match inventory_path() {
        Some(path) => Inventory::load(&path),
        None => Ok(None),
    }
}

/// Every repo with a recipes file.
pub fn recipe_repos() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(crate::recipe::local_dir()) else {
        return Vec::new();
    };
    let mut repos: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".toml")
                .map(str::to_string)
        })
        .collect();
    repos.sort();
    repos
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEEN: &str = "noise before\n\
        DIBS-PROBE sys user=box host=box os=Linux arch=x86_64\n\
        DIBS-PROBE bash version=5.2\n\
        DIBS-PROBE tools flock=/usr/bin/flock timeout=/usr/bin/timeout gtimeout= rsync=3.2.7 git=/usr/bin/git\n\
        DIBS-PROBE rust rustup=/h/.cargo/bin/rustup toolchains=stable-x86_64-unknown-linux-gnu,1.98.1-x86_64-unknown-linux-gnu\n\
        DIBS-PROBE gpu nvidia=610.57.04 nvcc=13.1 vulkan=yes\n\
        DIBS-PROBE account nopasswd=yes groups=box,render,sudo\n\
        DIBS-PROBE keys SHA256:alice SHA256:stranger SHA256:carol\n\
        DIBS-PROBE repos burn=https://x/burn.git cubecl=\n\
        DIBS-PROBE disk free_kb=123\n";

    fn machine(profiles: Vec<Profile>) -> Machine {
        Machine {
            provisioned: Provisioned {
                by: Provisioner::Hand,
                source: None,
            },
            ssh: None,
            paths: Vec::new(),
            people: vec!["alice".into(), "bob".into()],
            login: Login::Keys,
            profiles,
            repos: None,
        }
    }

    fn cx() -> Context {
        Context {
            owners: [
                ("SHA256:alice", "alice"),
                ("SHA256:bob", "bob"),
                ("SHA256:carol", "carol"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
            pins: [
                ("app".to_string(), "1.98.1".to_string()),
                ("old".to_string(), "1.80.0".to_string()),
            ]
            .into_iter()
            .collect(),
            recipe_repos: vec!["burn".into(), "cubecl".into(), "app".into()],
        }
    }

    fn finding(fs: &[Finding], area: Area) -> &Finding {
        fs.iter().find(|f| f.area == area).unwrap()
    }

    #[test]
    fn the_probe_is_posix_sh_and_reports_every_section() {
        let syntax = Command::new("sh")
            .args(["-n", "-c", PROBE])
            .status()
            .unwrap();
        assert!(syntax.success());
        let out = Command::new("sh").args(["-c", PROBE]).output().unwrap();
        let o = Observed::parse(&String::from_utf8_lossy(&out.stdout));
        for key in [
            "sys.os",
            "bash.version",
            "account.nopasswd",
            "login.tailscale_ssh",
            "disk.free_kb",
        ] {
            assert!(
                o.has(key),
                "{key} missing from:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        assert!(
            String::from_utf8_lossy(&out.stdout).lines().count() <= 20,
            "a job's digest keeps its first 20 lines"
        );
    }

    #[test]
    fn keys_are_matched_by_owner_and_a_stranger_or_an_unlisted_person_is_flagged() {
        let m = machine(vec![]);
        let a = m.access(&Observed::parse(SEEN), &cx());
        assert_eq!(
            (
                a.people.get("alice"),
                a.people.get("bob"),
                a.people.get("carol"),
                a.strangers.as_slice()
            ),
            (
                Some(&Standing::Key),
                Some(&Standing::Missing),
                Some(&Standing::Unlisted),
                ["SHA256:stranger".to_string()].as_slice()
            )
        );
        let f = m.keys(&a);
        assert_eq!(f.area, Area::Login);
        assert!(!f.ok);
        assert_eq!(
            f.detail,
            "no key of bob, carol's key, and carol is not listed here, a key of nobody listed: SHA256:stranger"
        );
    }

    #[test]
    fn a_machine_logged_into_by_tailscale_needs_it_on_and_no_keys_beside_it() {
        let mut m = machine(vec![]);
        m.login = Login::Tailscale;
        let without = Observed::parse(&SEEN.replace(
            "DIBS-PROBE keys SHA256:alice SHA256:stranger SHA256:carol",
            "DIBS-PROBE keys",
        ));
        assert_eq!(
            finding(&m.check(&without, &cx()), Area::Login).detail,
            "Tailscale SSH is off"
        );
        let on = SEEN.to_string() + "DIBS-PROBE login tailscale_ssh=yes\n";
        assert_eq!(
            finding(&m.check(&Observed::parse(&on), &cx()), Area::Login).detail,
            "a second way in, keys in authorized_keys: alice, carol, SHA256:stranger"
        );
        let clean = on.replace(
            "DIBS-PROBE keys SHA256:alice SHA256:stranger SHA256:carol",
            "DIBS-PROBE keys",
        );
        assert!(finding(&m.check(&Observed::parse(&clean), &cx()), Area::Login).ok);
    }

    #[test]
    fn toolchains_come_from_the_pins_of_the_repos_a_machine_needs() {
        let o = Observed::parse(SEEN);
        let mut m = machine(vec![Profile::Rust]);
        assert!(
            m.check(&o, &cx())
                .iter()
                .any(|f| f.area == Area::Rust && f.ok),
            "stable and app's pin are there"
        );
        m.repos = Some(vec!["old".into()]);
        assert_eq!(
            finding(&m.check(&o, &cx()), Area::Rust).detail,
            "no 1.80.0 toolchain"
        );
    }

    #[test]
    fn a_missing_clone_and_privileges_are_found() {
        let fs = machine(vec![Profile::Unprivileged, Profile::Dibs, Profile::Cuda])
            .check(&Observed::parse(SEEN), &cx());
        assert_eq!(finding(&fs, Area::Repos).detail, "no clone of app");
        assert_eq!(
            finding(&fs, Area::Account).detail,
            "sudo without a password, in sudo"
        );
        assert!(finding(&fs, Area::Dibs).ok && finding(&fs, Area::Cuda).ok);
    }

    #[test]
    fn an_old_bash_and_a_missing_timeout_fail_the_dibs_profile() {
        let o = Observed::parse(
            &SEEN
                .replace("version=5.2", "version=3.2")
                .replace("timeout=/usr/bin/timeout", "timeout="),
        );
        assert_eq!(
            finding(&machine(vec![Profile::Dibs]).check(&o, &cx()), Area::Dibs).detail,
            "GNU timeout, bash 3.2, needs 5.1"
        );
    }

    #[test]
    fn a_pin_is_read_from_either_toolchain_file() {
        let dir = std::env::temp_dir().join(format!("dibs-fleet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(pin(&dir), None);
        std::fs::write(dir.join("rust-toolchain"), "1.80.0\n").unwrap();
        assert_eq!(pin(&dir).as_deref(), Some("1.80.0"));
        std::fs::write(
            dir.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.98.1\"\n",
        )
        .unwrap();
        assert_eq!(pin(&dir).as_deref(), Some("1.98.1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
