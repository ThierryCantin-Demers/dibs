//! What each machine should have, read against what it has.
//!
//! The inventory says how to reach a machine and `--check` what is in it, but nothing said what
//! it should have, so a machine set up by hand drifted from the others unseen until a job failed
//! on it.

use crate::{
    call::{Asked, Bound, MachineCall},
    caller::Caller,
    cli::Call,
    inventory::{Inventory, InventoryError},
    machine::Kept,
    paths::{FileError, Paths},
    recipe::{Checkouts, Manifest, RepoError},
};
use dibs_format::{
    Exit, Label, MachineName, Mode,
    fleet::{Access, Area, Facts, Finding, Overview, PathCheck, Provisioned, Report, Standing},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::Duration,
};

/// Groups whose members are root in all but name.
const PRIVILEGED_GROUPS: [&str; 4] = ["sudo", "admin", "wheel", "docker"];
/// How long a machine has to answer its probe.
const PROBE_WAIT_SECONDS: u64 = 30;

/// Why `dibs machines` could not read what the machines should have.
#[derive(Debug)]
pub enum FleetError {
    NoHome,
    Unread(FileError),
    Parse {
        path: PathBuf,
        error: Box<toml::de::Error>,
    },
    /// A machine lists someone fleet.toml has no `[person]` for.
    Stranger {
        path: PathBuf,
        machine: String,
        person: String,
    },
    NoKeygen(io::Error),
    Repo(RepoError),
    Inventory(InventoryError),
    NotAKey {
        file: PathBuf,
        who: String,
    },
    NoMachine {
        name: String,
        path: PathBuf,
        have: Vec<String>,
    },
    Json(serde_json::Error),
}

/// Why nothing was read from a machine.
#[derive(Debug)]
enum Unprobed {
    /// Not in the inventory, so no runner of this dibs is there to ask; with how it is reached.
    NotInPool(Option<String>),
    /// No answer within the probe's bound.
    Late,
    Unreachable(String),
    Failed {
        exit: i32,
        said: String,
    },
    Unreadable(serde_json::Error),
}

/// Why a name a machine is reached by does not reach it from here.
#[derive(Debug)]
enum Unreached {
    Unresolved(String),
    NoAnswer(String),
}

impl fmt::Display for FleetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FleetError::NoHome => f.write_str("no HOME to find fleet.toml under"),
            FleetError::Unread(e) => write!(
                f,
                "{e}\n  It says what each machine should have: people and their keys, and per machine how it was set\n  \
                 up, the names it is reached by, who may log in and the profiles it needs.\n  \
                 docs/guide.md has its format, under \"What each machine should have\"."
            ),
            FleetError::Parse { path, error } => write!(f, "{}: {error}", path.display()),
            FleetError::Stranger {
                path,
                machine,
                person,
            } => write!(
                f,
                "{}: machine {machine} lists {person}, who has no [person.{person}]",
                path.display()
            ),
            FleetError::NoKeygen(e) => write!(f, "ssh-keygen: {e}"),
            FleetError::NotAKey { file, who } => {
                write!(f, "{}: not a public key ({who})", file.display())
            }
            FleetError::NoMachine { name, path, have } => write!(
                f,
                "no machine {name} in {}; it has: {}",
                path.display(),
                have.join(", ")
            ),
            FleetError::Json(e) => e.fmt(f),
            FleetError::Repo(e) => e.fmt(f),
            FleetError::Inventory(e) => e.fmt(f),
        }
    }
}

impl From<RepoError> for FleetError {
    fn from(e: RepoError) -> FleetError {
        FleetError::Repo(e)
    }
}

impl From<InventoryError> for FleetError {
    fn from(e: InventoryError) -> FleetError {
        FleetError::Inventory(e)
    }
}

impl fmt::Display for Unprobed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unprobed::NotInPool(Some(ssh)) => write!(
                f,
                "not in the pool: dibs --check {ssh} --write records it and installs what probes it"
            ),
            Unprobed::NotInPool(None) => f.write_str(
                "not in the pool, and no ssh to reach it by: give it one, or record it with dibs --check",
            ),
            Unprobed::Late => write!(f, "no answer within {PROBE_WAIT_SECONDS}s"),
            Unprobed::Unreachable(said) => write!(f, "unreachable: {said}"),
            Unprobed::Failed { exit, said } => {
                write!(f, "the probe failed (exit {exit}): {said}")
            }
            Unprobed::Unreadable(e) => write!(f, "the probe printed no facts: {e}"),
        }
    }
}

impl fmt::Display for Unreached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unreached::Unresolved(name) => write!(f, "{name} does not resolve here"),
            Unreached::NoAnswer(name) => {
                write!(f, "{name} resolves, and nothing answers on port 22")
            }
        }
    }
}

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
    /// How to reach a machine not yet in the pool, for the `--check` that adds it.
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

/// A machine as the survey checks it: its entry, what was found on it, and what it is checked
/// against.
struct Surveyed<'a> {
    machine: &'a Machine,
    facts: &'a Facts,
    cx: &'a Context,
}

/// What a machine is checked against, beyond its own entry.
struct Context {
    /// Each key's fingerprint, to whom it belongs.
    owners: BTreeMap<String, String>,
    /// The toolchain a repo pins; a repo absent here builds on stable.
    pins: BTreeMap<String, String>,
    recipe_repos: Vec<String>,
}

impl<'a> Surveyed<'a> {
    fn repos(&self) -> &'a [String] {
        self.machine
            .repos
            .as_deref()
            .unwrap_or(&self.cx.recipe_repos)
    }

    fn check(&self) -> Vec<Finding> {
        let login = match self.machine.login {
            Login::Keys => self.keys(&self.access()),
            Login::Tailscale => self.tailscale(),
        };
        let mut out = vec![login, self.repo_clones()];
        out.extend(self.machine.profiles.iter().map(|p| p.check(self)));
        out
    }

    fn access(&self) -> Access {
        let holds = |p: &str| {
            self.facts
                .keys
                .iter()
                .any(|fp| self.cx.owners.get(fp).is_some_and(|who| who == p))
        };
        let mut people: BTreeMap<String, Standing> = self
            .machine
            .people
            .iter()
            .map(|p| {
                let standing = match (self.machine.login, holds(p)) {
                    (Login::Tailscale, _) => Standing::Tailnet,
                    (Login::Keys, true) => Standing::Key,
                    (Login::Keys, false) => Standing::Missing,
                };
                (p.clone(), standing)
            })
            .collect();
        let mut strangers = Vec::new();
        for fp in &self.facts.keys {
            match self.cx.owners.get(fp) {
                None => strangers.push(fp.clone()),
                Some(who) if !self.machine.people.contains(who) => {
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
            format!("keys of {}", self.machine.people.join(", ")),
        )
    }

    fn tailscale(&self) -> Finding {
        let mut missing = Vec::new();
        if !self.facts.tailscale_ssh {
            missing.push("Tailscale SSH is off".to_string());
        }
        if !self.facts.keys.is_empty() {
            let whose: Vec<&str> = self
                .facts
                .keys
                .iter()
                .map(|fp| self.cx.owners.get(fp).map_or(fp.as_str(), String::as_str))
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

    fn repo_clones(&self) -> Finding {
        let wanted = self.repos();
        let missing = wanted
            .iter()
            .filter(|r| !self.facts.repos.contains_key(*r))
            .map(|r| format!("no clone of {r}"))
            .collect();
        Finding::new(Area::Repos, missing, wanted.join(", "))
    }
}

impl Profile {
    fn check(self, surveyed: &Surveyed) -> Finding {
        let o = surveyed.facts;
        match self {
            Profile::Dibs => {
                let wants = [
                    (o.bash.is_none(), "no bash"),
                    (o.rsync.is_none(), "no rsync 3"),
                    (!o.git, "no git"),
                    (!o.cargo, "no cargo"),
                ];
                let missing = wants
                    .iter()
                    .filter(|(lacks, _)| *lacks)
                    .map(|(_, said)| said.to_string())
                    .collect();
                Finding::new(
                    Area::Dibs,
                    missing,
                    format!(
                        "bash {}, rsync {}",
                        o.bash.as_deref().unwrap_or_default(),
                        o.rsync.as_deref().unwrap_or_default()
                    ),
                )
            }
            Profile::Rust => {
                if !o.rustup {
                    return Finding::new(Area::Rust, vec!["no rustup".into()], String::new());
                }
                let have = &o.toolchains;
                let needed: BTreeSet<&str> = std::iter::once("stable")
                    .chain(
                        surveyed
                            .repos()
                            .iter()
                            .filter_map(|r| surveyed.cx.pins.get(r).map(String::as_str)),
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
                if o.nvidia.is_none() {
                    missing.push("no NVIDIA driver".into());
                }
                if o.nvcc.is_none() {
                    missing.push("no nvcc".into());
                }
                Finding::new(
                    Area::Cuda,
                    missing,
                    format!(
                        "driver {}, nvcc {}",
                        o.nvidia.as_deref().unwrap_or_default(),
                        o.nvcc.as_deref().unwrap_or_default()
                    ),
                )
            }
            Profile::Vulkan => {
                let missing = (!o.vulkan)
                    .then(|| "no Vulkan loader".to_string())
                    .into_iter()
                    .collect();
                Finding::new(Area::Vulkan, missing, "loader present".into())
            }
            Profile::Metal => {
                let missing = (o.os != "Darwin")
                    .then(|| format!("{} has no Metal", o.os))
                    .into_iter()
                    .collect();
                Finding::new(Area::Metal, missing, "macOS".into())
            }
            Profile::Unprivileged => {
                let mut missing = Vec::new();
                if o.nopasswd {
                    missing.push("sudo without a password".to_string());
                }
                let groups: Vec<&str> = o
                    .groups
                    .iter()
                    .map(String::as_str)
                    .filter(|g| PRIVILEGED_GROUPS.contains(g))
                    .collect();
                if !groups.is_empty() {
                    missing.push(format!("in {}", groups.join(", ")));
                }
                Finding::new(Area::Account, missing, format!("{}, no root", o.user))
            }
        }
    }
}

impl Fleet {
    /// `DIBS_FLEET`, or beside the inventory.
    fn path() -> Result<PathBuf, FleetError> {
        Paths::from_env().fleet().ok_or(FleetError::NoHome)
    }

    fn load(path: &Path) -> Result<Fleet, FleetError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| FleetError::Unread(FileError::new(path, e)))?;
        let fleet: Fleet = toml::from_str(&text).map_err(|error| FleetError::Parse {
            path: path.to_path_buf(),
            error: Box::new(error),
        })?;
        for (name, m) in &fleet.machine {
            if let Some(p) = m.people.iter().find(|p| !fleet.person.contains_key(*p)) {
                return Err(FleetError::Stranger {
                    path: path.to_path_buf(),
                    machine: name.clone(),
                    person: p.clone(),
                });
            }
        }
        Ok(fleet)
    }
}

/// Each key file's fingerprint, read the way the machine's side reads its `authorized_keys`.
fn owners(fleet: &Fleet, dir: &Path) -> Result<BTreeMap<String, String>, FleetError> {
    let mut out = BTreeMap::new();
    for (who, person) in &fleet.person {
        for key in &person.keys {
            let file = dir.join(key);
            let listed = Command::new("ssh-keygen")
                .arg("-lf")
                .arg(&file)
                .output()
                .map_err(FleetError::NoKeygen)?;
            if !listed.status.success() {
                return Err(FleetError::NotAKey {
                    file,
                    who: who.clone(),
                });
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

/// What a machine in the pool has, as the check of its runner reads it, or why it said nothing.
fn probe(name: &str, m: &Machine, pool: &BTreeSet<String>) -> Result<Facts, Unprobed> {
    if !pool.contains(name) {
        return Err(Unprobed::NotInPool(m.ssh.clone()));
    }
    let machine = MachineName::new(name);
    let call = Call {
        on: Some(machine.clone()),
        ..Call::default()
    };
    let flags = Call {
        json: true,
        ..Call::default()
    };
    let caller = Caller::from_env();
    let answer = match MachineCall::new(&call, &caller) {
        Ok(asking) => asking.ask(
            &machine,
            &flags,
            Asked::plain(Mode::Check, Label::new("check")),
            Bound::polled(PROBE_WAIT_SECONDS, Kept::Everything),
        ),
        Err(e) => {
            return Err(Unprobed::Failed {
                exit: e.exit(),
                said: e.to_string().trim().to_string(),
            });
        }
    };
    let output = String::from_utf8_lossy(&answer.output);
    let said = || {
        output
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let unreachable = i32::from(Exit::Unreachable.code());
    match answer.exit {
        None => Err(Unprobed::Late),
        Some(0) => serde_json::from_str(
            output
                .lines()
                .find(|l| l.starts_with('{'))
                .unwrap_or_default(),
        )
        .map_err(Unprobed::Unreadable),
        Some(code) if code == unreachable => Err(Unprobed::Unreachable(said())),
        Some(exit) => Err(Unprobed::Failed { exit, said: said() }),
    }
}

/// From here: the name resolves, and something answers on the ssh port.
fn reach(name: &str) -> Result<(), Unreached> {
    let addrs: Vec<_> = (name, 22)
        .to_socket_addrs()
        .map_err(|_| Unreached::Unresolved(name.to_string()))?
        .collect();
    match addrs
        .iter()
        .any(|a| TcpStream::connect_timeout(a, Duration::from_secs(3)).is_ok())
    {
        true => Ok(()),
        false => Err(Unreached::NoAnswer(name.to_string())),
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
                problem: t
                    .join()
                    .expect("a reach does not panic")
                    .err()
                    .map(|e| e.to_string()),
            })
            .collect()
    })
}

fn report(name: &str, m: &Machine, cx: &Context, pool: &BTreeSet<String>) -> Report {
    let observed = probe(name, m, pool);
    let paths = reach_all(&m.paths);
    let problems = paths.iter().filter_map(|p| p.problem.clone()).collect();
    let mut findings = vec![Finding::new(Area::Paths, problems, m.paths.join(", "))];
    let (unprobed, access) = match observed {
        Ok(o) => {
            let surveyed = Surveyed {
                machine: m,
                facts: &o,
                cx,
            };
            findings.extend(surveyed.check());
            (None, surveyed.access())
        }
        Err(e) => (Some(e.to_string()), Access::default()),
    };
    Report {
        machine: name.to_string(),
        provisioned: m.provisioned.clone(),
        unprobed,
        paths,
        access,
        findings,
    }
}

fn render(reports: &[Report]) -> String {
    let mut s = String::new();
    for r in reports {
        s += &format!("{}  set up by {}\n", r.machine, r.provisioned);
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

/// `dibs machines [<machine>]`: the overview as text, or as JSON, exiting 1 when anything is
/// missing.
pub fn command(
    json: bool,
    only: Option<&str>,
    root: &Path,
    recipe_repos: Vec<String>,
    pool: &BTreeSet<String>,
) -> Result<ExitCode, FleetError> {
    let overview = overview(only, root, recipe_repos, pool)?;
    match json {
        true => println!(
            "{}",
            serde_json::to_string_pretty(&overview).map_err(FleetError::Json)?
        ),
        false => print!("{}", render(&overview.machines)),
    }
    Ok(match overview.machines.iter().all(Report::as_expected) {
        true => ExitCode::SUCCESS,
        false => ExitCode::from(Exit::Failed.code()),
    })
}

/// The overview as `dibs machines` takes it, from this computer's own settings.
pub fn survey(only: Option<&str>) -> Result<Overview, FleetError> {
    overview(
        only,
        Checkouts::here()?.root(),
        Manifest::local_repos(),
        &Inventory::pool()?,
    )
}

/// Every machine in fleet.toml, or the one named, probed at once.
fn overview(
    only: Option<&str>,
    root: &Path,
    recipe_repos: Vec<String>,
    pool: &BTreeSet<String>,
) -> Result<Overview, FleetError> {
    let path = Fleet::path()?;
    let mut fleet = Fleet::load(&path)?;
    if let Some(name) = only {
        if !fleet.machine.contains_key(name) {
            return Err(FleetError::NoMachine {
                name: name.to_string(),
                path,
                have: fleet.machine.into_keys().collect(),
            });
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
    Ok(Overview {
        people: fleet.person.into_keys().collect(),
        machines: reports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dibs_format::fleet::Provisioner;

    fn seen() -> Facts {
        Facts {
            user: "box".into(),
            host: "box".into(),
            os: "Linux".into(),
            arch: "x86_64".into(),
            bash: Some("5.2.26".into()),
            rsync: Some("3.2.7".into()),
            git: true,
            cargo: true,
            rustup: true,
            toolchains: vec![
                "stable-x86_64-unknown-linux-gnu".into(),
                "1.98.1-x86_64-unknown-linux-gnu".into(),
            ],
            nvidia: Some("610.57.04".into()),
            nvcc: Some("13.1".into()),
            vulkan: true,
            nopasswd: true,
            groups: vec!["box".into(), "render".into(), "sudo".into()],
            keys: vec![
                "SHA256:alice".into(),
                "SHA256:carol".into(),
                "SHA256:stranger".into(),
            ],
            tailscale_ssh: false,
            repos: [("burn", "https://x/burn.git"), ("cubecl", "")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

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

    fn checked(m: &Machine, o: &Facts) -> Vec<Finding> {
        Surveyed {
            machine: m,
            facts: o,
            cx: &cx(),
        }
        .check()
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
    fn a_machine_outside_the_pool_is_not_probed() {
        assert!(matches!(
            probe("away", &machine(vec![]), &BTreeSet::new()),
            Err(Unprobed::NotInPool(None))
        ));
    }

    #[test]
    fn keys_are_matched_by_owner_and_a_stranger_or_an_unlisted_person_is_flagged() {
        let (m, o, cx) = (machine(vec![]), seen(), cx());
        let surveyed = Surveyed {
            machine: &m,
            facts: &o,
            cx: &cx,
        };
        let a = surveyed.access();
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
        let f = surveyed.keys(&a);
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
        let without = Facts {
            keys: Vec::new(),
            ..seen()
        };
        assert_eq!(
            finding(&checked(&m, &without), Area::Login).detail,
            "Tailscale SSH is off"
        );
        let on = Facts {
            tailscale_ssh: true,
            ..seen()
        };
        assert_eq!(
            finding(&checked(&m, &on), Area::Login).detail,
            "a second way in, keys in authorized_keys: alice, carol, SHA256:stranger"
        );
        let clean = Facts {
            keys: Vec::new(),
            ..on
        };
        assert!(finding(&checked(&m, &clean), Area::Login).ok);
    }

    #[test]
    fn toolchains_come_from_the_pins_of_the_repos_a_machine_needs() {
        let o = seen();
        let mut m = machine(vec![Profile::Rust]);
        assert!(
            checked(&m, &o).iter().any(|f| f.area == Area::Rust && f.ok),
            "stable and app's pin are there"
        );
        m.repos = Some(vec!["old".into()]);
        assert_eq!(
            finding(&checked(&m, &o), Area::Rust).detail,
            "no 1.80.0 toolchain"
        );
    }

    #[test]
    fn a_missing_clone_and_privileges_are_found() {
        let fs = checked(
            &machine(vec![Profile::Unprivileged, Profile::Dibs, Profile::Cuda]),
            &seen(),
        );
        assert_eq!(finding(&fs, Area::Repos).detail, "no clone of app");
        assert_eq!(
            finding(&fs, Area::Account).detail,
            "sudo without a password, in sudo"
        );
        assert!(finding(&fs, Area::Dibs).ok && finding(&fs, Area::Cuda).ok);
    }

    #[test]
    fn the_dibs_profile_wants_what_a_runner_and_its_jobs_need() {
        let o = Facts {
            rsync: None,
            cargo: false,
            ..seen()
        };
        assert_eq!(
            finding(&checked(&machine(vec![Profile::Dibs]), &o), Area::Dibs).detail,
            "no rsync 3, no cargo"
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
