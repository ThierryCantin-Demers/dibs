//! `dibs dispatch`: work handed to the dibspatcher, which keeps it alive after this computer goes.
//! It is reached over ssh as `dibspatcher.toml` names it, and speaks the versioned API in
//! `dibs_format::dibspatcher`.

use crate::{
    caller::Caller,
    execution::{Carried, Repo},
    git::Git,
    recipe::Checkouts,
};
use dibs_format::{
    Exit, Hex as _, Moment,
    dibspatcher::{
        API_COMMAND, Ask, Call, Detail, NamedCall, Reply, STAGE_PREFIX, Seal, SplitCall,
        Submission, Summary, Upload, VERSION, Work,
    },
};
use std::{
    collections::BTreeMap,
    io::{BufRead as _, BufReader, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The word `dibs` takes these verbs under.
pub const WORD: &str = "dispatch";

/// Seconds a follow waits before it checks in, unless `DIBS_CHECK_IN` says.
const CHECK_IN: u64 = 1800;
/// Times a call is made again with the same key when ssh lost it.
const RETRIES: usize = 3;
/// What ssh exits with when it lost the connection or never had one.
const SSH_FAILED: i32 = 255;

const USAGE: &str = "usage:
  dibs dispatch bench <repo>@local <recipe> [--<value> <v>]... [--no-follow] [--key <key>]
                                   a split recipe: built on the compile pool, measured on the GPU pool
  dibs dispatch run --on <machine> [--bench --device <card>] [--label <l>] [--max <s>] [--no-follow] [--key <key>] <command>
                                   a call on one machine, kept alive by the dibspatcher
  dibs dispatch list [--all]           your submissions, or everyone's
  dibs dispatch status <id>            how each step went
  dibs dispatch out <id> [<step>] [--job]
                                   what each step said; --job reads the job's whole log
  dibs dispatch follow <id>            waits for it to end, or checks in after 30 minutes
  dibs dispatch kill <id>              stops it
  dibs dispatch machines               the dibspatcher's machines and pools";

pub fn serve(words: &[String]) -> i32 {
    let dibspatcher = match Dibspatcher::configured() {
        Ok(o) => o,
        Err(why) => {
            eprintln!("dibs: {why}");
            return Exit::Refused.status();
        }
    };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    if let ["run", rest @ ..] = words.as_slice() {
        return named(rest)
            .and_then(|(work, sent)| dibspatcher.submit(work, None, sent))
            .unwrap_or_else(|why| {
                eprintln!("dibs: {why}");
                Exit::Refused.status()
            });
    }
    let mut follow = true;
    let mut key = None;
    let mut given = Vec::new();
    let mut words = words.into_iter();
    while let Some(word) = words.next() {
        match word {
            "--no-follow" => follow = false,
            "--key" => match words.next() {
                Some(k) => key = Some(k.to_string()),
                None => {
                    eprintln!("dibs: --key needs the key a submission was sent under");
                    return Exit::Refused.status();
                }
            },
            word => given.push(word),
        }
    }
    let sent = Sent { key, follow };
    let result = match given.as_slice() {
        ["bench", target, recipe, values @ ..] => split(target, recipe, values)
            .and_then(|(work, upload)| dibspatcher.submit(work, Some(upload), sent)),
        ["list"] => dibspatcher.list(false),
        ["list", "--all"] => dibspatcher.list(true),
        ["status", id] => dibspatcher.status(id),
        ["out", id] => dibspatcher.out(id, None, false),
        ["out", id, "--job"] => dibspatcher.out(id, None, true),
        ["out", id, step] => dibspatcher.out(id, Some(step), false),
        ["out", id, step, "--job"] => dibspatcher.out(id, Some(step), true),
        ["follow", id] => dibspatcher.follow(id),
        ["kill", id] => dibspatcher.ask(Call::Kill { id: id.to_string() }, &mut Shown::default()),
        ["machines"] => dibspatcher.ask(Call::Inventory, &mut Shown::default()),
        [] | ["-h"] | ["--help"] | ["help"] => {
            println!("{USAGE}");
            return 0;
        }
        _ => Err(format!("dibs dispatch takes:\n{USAGE}")),
    };
    result.unwrap_or_else(|why| {
        eprintln!("dibs: {why}");
        Exit::Refused.status()
    })
}

/// How a submission is sent: under the key given, so one sent before is asked after rather than
/// sent again, or a new one; then followed, or not.
struct Sent {
    key: Option<String>,
    follow: bool,
}

fn split(target: &str, recipe: &str, values: &[&str]) -> Result<(Work, (Upload, PathBuf)), String> {
    let Some(repo) = target.strip_suffix("@local") else {
        return Err(format!(
            "{target}: a split through the dibspatcher is built from your working tree, as <repo>@local"
        ));
    };
    let mut given = BTreeMap::new();
    let mut words = values.iter();
    while let Some(word) = words.next() {
        let Some(name) = word.strip_prefix("--") else {
            return Err(format!("{word}: a recipe's values are --<name> <value>"));
        };
        let (name, value) = match name.split_once('=') {
            Some((name, value)) => (name.to_string(), value.to_string()),
            None => (
                name.to_string(),
                words
                    .next()
                    .ok_or_else(|| format!("--{name} needs a value"))?
                    .to_string(),
            ),
        };
        given.insert(name, value);
    }
    let dir = Checkouts::here()
        .and_then(|c| c.find(repo))
        .map_err(|e| e.to_string())?;
    let name = Repo(&dir).identity();
    let work = Work::Split(SplitCall {
        repo: name.clone(),
        recipe: recipe.to_string(),
        values: given,
    });
    let upload = Upload {
        stage: String::new(),
        repo: name,
        head: String::new(),
        digest: String::new(),
        files: 0,
    };
    let upload = Sending { dir: dir.clone() }.prepared(upload)?;
    Ok((work, (upload, dir)))
}

fn named(words: &[&str]) -> Result<(Work, Sent), String> {
    let mut call = NamedCall {
        on: String::new(),
        bench: false,
        device: None,
        label: String::new(),
        command: String::new(),
        max: None,
    };
    let mut sent = Sent {
        key: None,
        follow: true,
    };
    let mut words = words.iter();
    let mut command = Vec::new();
    while let Some(word) = words.next() {
        if !command.is_empty() {
            command.push(word.to_string());
            continue;
        }
        let mut value = |flag: &str| {
            words
                .next()
                .map(|v| v.to_string())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match *word {
            "--on" => call.on = value("--on")?,
            "--bench" => call.bench = true,
            "--no-follow" => sent.follow = false,
            "--key" => sent.key = Some(value("--key")?),
            "--device" => call.device = Some(value("--device")?),
            "--label" => call.label = value("--label")?,
            "--max" => {
                call.max = Some(
                    value("--max")?
                        .parse()
                        .map_err(|_| "--max takes seconds".to_string())?,
                )
            }
            "--" => {
                command.extend(words.by_ref().map(|w| w.to_string()));
                break;
            }
            flag if flag.starts_with("--") => {
                return Err(format!("dibs dispatch run takes no {flag}"));
            }
            word => command.push(word.to_string()),
        }
    }
    if call.on.is_empty() {
        return Err("dibs dispatch run names its machine with --on".into());
    }
    if command.is_empty() {
        return Err("dibs dispatch run needs a command".into());
    }
    call.command = crate::cli::Command(command).shell_string();
    if call.label.is_empty() {
        call.label = std::env::current_dir()
            .ok()
            .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "dispatch".into());
    }
    Ok((Work::Call(call), sent))
}

/// A working tree as it is sent: its files by its ignore rules and what it carries past them,
/// then its git directory.
struct Sending {
    dir: PathBuf,
}

impl Sending {
    fn files(&self) -> Result<Vec<String>, String> {
        let listed = Git(&self.dir)
            .run(&["ls-files", "-co", "--exclude-standard", "-z"])
            .map_err(|e| e.to_string())?;
        let carried = Carried::of(&self.dir)
            .files(&self.dir)
            .map_err(|e| e.to_string())?;
        let mut files: Vec<String> = listed
            .split('\0')
            .map(str::to_string)
            .chain(carried)
            .filter(|f| {
                !f.is_empty()
                    && std::fs::symlink_metadata(self.dir.join(f))
                        .is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
            })
            .collect();
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// The upload's seal and head, computed before anything is sent: what the dibspatcher must
    /// find when it arrives.
    fn prepared(&self, mut upload: Upload) -> Result<Upload, String> {
        let files = self.files()?;
        let (digest, count) = Seal::of(&self.dir, files.iter().map(String::as_str))
            .map_err(|e| format!("{} could not be read to be sent: {e}", self.dir.display()))?;
        upload.digest = digest;
        upload.files = count;
        upload.head = Git(&self.dir)
            .run(&["rev-parse", "HEAD"])
            .map_err(|e| e.to_string())?
            .trim()
            .to_string();
        Ok(upload)
    }
}

/// The team's inventory files from the dibspatcher, by their path under its config directory;
/// None when no dibspatcher is set up here.
/// The account the dibspatcher set up here is reached as, which a copy of its inventory is tied
/// to.
pub fn dibspatcher_ssh() -> Option<String> {
    Dibspatcher::configured().ok().map(|d| d.ssh)
}

pub fn team_files() -> Option<Result<BTreeMap<String, String>, String>> {
    let dibspatcher = Dibspatcher::configured().ok()?;
    let mut shown = Shown {
        keeps_files: true,
        ..Shown::default()
    };
    Some(match dibspatcher.ask(Call::Inventory, &mut shown) {
        Ok(0) => shown
            .files
            .filter(|files| !files.is_empty())
            .ok_or_else(|| format!("the dibspatcher at {} sent no inventory", dibspatcher.ssh)),
        Ok(code) => Err(format!(
            "the dibspatcher at {} did not answer (exit {code})",
            dibspatcher.ssh
        )),
        Err(e) => Err(e),
    })
}

/// Where the dibspatcher is: `DIBS_DISPATCHER`, else `dibspatcher.toml` beside the
/// inventory, `ssh = "<destination>"` and optionally `ssh_config = "<file>"`.
pub struct Dibspatcher {
    ssh: String,
    config: Option<PathBuf>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Configured {
    ssh: String,
    ssh_config: Option<String>,
}

impl Dibspatcher {
    fn configured() -> Result<Dibspatcher, String> {
        if let Some(ssh) = std::env::var("DIBS_DISPATCHER")
            .ok()
            .filter(|s| !s.is_empty())
        {
            return Ok(Dibspatcher { ssh, config: None });
        }
        let path = crate::paths::Paths::from_env()
            .inventory()
            .and_then(|i| i.parent().map(|d| d.join("dibspatcher.toml")))
            .ok_or("no config directory to find dibspatcher.toml in")?;
        let text = std::fs::read_to_string(&path).map_err(|_| {
            format!(
                "no dibspatcher is set up here: {} names it, as ssh = \"<account>@<host>\"",
                path.display()
            )
        })?;
        let configured: Configured =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        Ok(Dibspatcher {
            ssh: configured.ssh,
            config: configured.ssh_config.map(|c| match c.strip_prefix("~/") {
                Some(rest) => home.join(rest),
                None => PathBuf::from(c),
            }),
        })
    }

    /// ssh's own words, before its destination.
    fn ssh_options(&self) -> Vec<String> {
        let mut options = Vec::new();
        if let Some(config) = &self.config {
            options.extend(["-F".to_string(), config.display().to_string()]);
        }
        options.extend(["-o".into(), "BatchMode=yes".into()]);
        options.extend(["-o".into(), "ConnectTimeout=10".into()]);
        options
    }

    /// One API call: the ask on ssh's stdin, each reply as it comes. The exit `End` gives, or
    /// ssh's own when it ended without one.
    fn ask(&self, call: Call, shown: &mut Shown) -> Result<i32, String> {
        let mut ssh = Command::new("ssh");
        ssh.args(self.ssh_options())
            .arg(&self.ssh)
            .arg(API_COMMAND)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = ssh
            .spawn()
            .map_err(|e| format!("ssh could not start: {e}"))?;
        let ask = Ask { v: VERSION, call };
        if let Some(mut stdin) = child.stdin.take() {
            let mut line = serde_json::to_vec(&ask).map_err(|e| e.to_string())?;
            line.push(b'\n');
            let _ = stdin.write_all(&line);
        }
        let mut exit = None;
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                match serde_json::from_str::<Reply>(&line) {
                    Ok(Reply::End { exit: code }) => exit = Some(code),
                    Ok(reply) => shown.show(reply),
                    Err(_) => eprintln!("{line}"),
                }
            }
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        Ok(exit.unwrap_or_else(|| Exit::shell_status(status)))
    }

    /// An ask made again with the same submission, so a lost connection never stores it twice.
    fn ask_again(&self, call: &Call, shown: &mut Shown) -> Result<i32, String> {
        let mut code = SSH_FAILED;
        for _ in 0..RETRIES {
            code = self.ask(call.clone(), shown)?;
            if code != SSH_FAILED || shown.accepted.is_some() {
                break;
            }
            eprintln!("dibs: lost the dibspatcher at {}; asking again", self.ssh);
        }
        Ok(code)
    }

    fn submit(
        &self,
        work: Work,
        upload: Option<(Upload, PathBuf)>,
        sent: Sent,
    ) -> Result<i32, String> {
        let key = sent.key.unwrap_or_else(key);
        let agent = Caller::from_env().name;
        let upload = match upload {
            Some((mut upload, dir)) => {
                upload.stage = key.clone();
                if let Some(code) = self.send(&upload, &dir)? {
                    return Ok(code);
                }
                Some(upload)
            }
            None => None,
        };
        let call = Call::Submit(Box::new(Submission {
            key: key.clone(),
            agent,
            work,
            upload,
        }));
        let mut shown = Shown::default();
        let code = self.ask_again(&call, &mut shown)?;
        let Some(id) = shown.accepted.clone() else {
            if code == SSH_FAILED {
                eprintln!(
                    "dibs: the dibspatcher at {} never said whether it took the submission, and may have.\n  \
                     The same command with --key {key} asks again, and is that submission if it was taken.",
                    self.ssh
                );
                return Ok(Exit::Unreachable.status());
            }
            return Ok(code);
        };
        if !sent.follow {
            println!("{id}");
            return Ok(0);
        }
        self.follow(&id)
    }

    /// The tree, then its git directory, into the coworker's stage on the dibspatcher, each
    /// against the copy the last upload of this repo left there. Some exit when one failed.
    fn send(&self, upload: &Upload, dir: &Path) -> Result<Option<i32>, String> {
        let dir = dir.to_path_buf();
        let sending = Sending { dir: dir.clone() };
        let files = sending.files()?;
        let list = std::env::temp_dir().join(format!("dibspatcher-{}.files", upload.stage));
        std::fs::write(&list, files.join("\0")).map_err(|e| e.to_string())?;
        let rsh = std::iter::once("ssh".to_string())
            .chain(self.ssh_options())
            .collect::<Vec<_>>()
            .join(" ");
        let stage = format!("{}:{STAGE_PREFIX}{}/", self.ssh, upload.stage);
        eprintln!(
            "dibs: sending {} ({} files and its git directory) to the dibspatcher",
            upload.repo, upload.files
        );
        let tree = [
            "-rlp".to_string(),
            "--checksum".into(),
            "--partial".into(),
            "--from0".into(),
            format!("--files-from={}", list.display()),
            format!("--copy-dest=../../last/{}", upload.repo),
            format!("{}/", dir.display()),
            stage.clone(),
        ];
        let sent = rsync(&rsh, &tree);
        let _ = std::fs::remove_file(&list);
        if sent != 0 {
            eprintln!(
                "dibs: {}'s files could not be sent to the dibspatcher",
                upload.repo
            );
            return Ok(Some(sent));
        }
        let common = Git(&dir)
            .run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .map_err(|e| e.to_string())?;
        let own = Git(&dir)
            .run(&["rev-parse", "--absolute-git-dir"])
            .map_err(|e| e.to_string())?;
        let (common, own) = (PathBuf::from(common.trim()), PathBuf::from(own.trim()));
        let git = format!("{stage}.git/");
        let git_dir = [
            "-rlp".to_string(),
            "--checksum".into(),
            "--partial".into(),
            format!("--copy-dest=../../../last/{}/.git", upload.repo),
            format!("{}/", common.display()),
            git.clone(),
        ];
        let mut sent = rsync(&rsh, &git_dir);
        if sent == 0 && own != common {
            let head: [&Path; 2] = [&own.join("HEAD"), &own.join("index")];
            let mut worktree = vec!["-lp".to_string()];
            worktree.extend(head.iter().map(|p| p.display().to_string()));
            worktree.push(git);
            sent = rsync(&rsh, &worktree);
        }
        if sent != 0 {
            eprintln!(
                "dibs: {}'s git directory could not be sent to the dibspatcher",
                upload.repo
            );
            return Ok(Some(sent));
        }
        Ok(None)
    }

    fn list(&self, all: bool) -> Result<i32, String> {
        self.ask(Call::List { all }, &mut Shown::default())
    }

    fn status(&self, id: &str) -> Result<i32, String> {
        self.ask(Call::Status { id: id.into() }, &mut Shown::default())
    }

    fn out(&self, id: &str, step: Option<&str>, job: bool) -> Result<i32, String> {
        self.ask(
            Call::Out {
                id: id.into(),
                step: step.map(String::from),
                job,
            },
            &mut Shown::default(),
        )
    }

    fn follow(&self, id: &str) -> Result<i32, String> {
        let check_in = std::env::var("DIBS_CHECK_IN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(CHECK_IN);
        let code = self.ask(
            Call::Follow {
                id: id.into(),
                check_in,
            },
            &mut Shown::default(),
        )?;
        if code == SSH_FAILED {
            eprintln!(
                "dibs: lost the dibspatcher while following {id}, which carries on there.\n  dibs dispatch follow {id}  follows it again"
            );
            return Ok(Exit::Unreachable.status());
        }
        Ok(code)
    }
}

fn rsync(rsh: &str, args: &[String]) -> i32 {
    Command::new("rsync")
        .arg("-e")
        .arg(rsh)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .map(Exit::shell_status)
        .unwrap_or(Exit::Failed.status())
}

/// A submission's key: when it was made here, and randomness, so a retry is the one submission.
fn key() -> String {
    let mut bytes = [0u8; 6];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        use std::io::Read as _;
        let _ = random.read_exact(&mut bytes);
    }
    format!("k{}-{}", Moment::now().compact(), bytes[..].hex())
}

/// What the dibspatcher's replies are shown as.
#[derive(Default)]
struct Shown {
    accepted: Option<String>,
    /// An inventory's files are kept, not printed.
    keeps_files: bool,
    files: Option<BTreeMap<String, String>>,
}

impl Shown {
    fn show(&mut self, reply: Reply) {
        match reply {
            Reply::Accepted { id, duplicate } => {
                eprintln!(
                    "dibs: submission {id} {} by the dibspatcher: it carries on whether or not this call does",
                    match duplicate {
                        true => "was already accepted",
                        false => "accepted",
                    }
                );
                self.accepted = Some(id);
            }
            Reply::Submissions(list) => {
                for summary in list {
                    println!("{}", line(&summary));
                }
            }
            Reply::Status(detail) => print!("{}", status(&detail)),
            Reply::Inventory { files, .. } if self.keeps_files => self.files = Some(files),
            Reply::Inventory {
                machines, pools, ..
            } => {
                for (pool, members) in pools {
                    println!("pool {pool}: {}", members.join(" "));
                }
                println!();
                print!("{machines}");
            }
            Reply::Text { err: true, text } => eprint!("{text}"),
            Reply::Text { err: false, text } => print!("{text}"),
            Reply::Trailer { step, trailer } => eprintln!("{step}: {trailer}"),
            Reply::Admitted { step, admitted } => eprintln!("{step}: {admitted:?}"),
            Reply::End { .. } => {}
        }
    }
}

fn line(summary: &Summary) -> String {
    format!(
        "{}  {:<11}  {:<6}  {}  {}  {}",
        summary.id,
        summary.state.as_str(),
        summary
            .exit
            .map(|e| format!("exit {e}"))
            .unwrap_or_default(),
        Moment::at(summary.created).minute(),
        summary.coworker,
        summary.what
    )
}

fn status(detail: &Detail) -> String {
    let mut text = format!("{}\n", line(&detail.summary));
    if let Some(note) = &detail.note {
        text.push_str(&format!("  {note}\n"));
    }
    for step in &detail.steps {
        text.push_str(&format!(
            "  {:<8} {:<11} {:<18} {}\n",
            step.name,
            step.state.as_str(),
            step.machine.as_deref().unwrap_or("dibspatcher"),
            match (&step.trailer, &step.note) {
                (Some(trailer), _) => trailer.to_string(),
                (None, Some(note)) => note.clone(),
                (None, None) => String::new(),
            }
        ));
        if let (Some(_), Some(note)) = (&step.trailer, &step.note) {
            text.push_str(&format!("           {note}\n"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_call_reads_its_flags_and_keeps_its_command_whole() {
        let (Work::Call(call), _) = named(&[
            "--on", "m", "--bench", "--device", "gpu:x", "--label", "l", "echo", "--on",
        ])
        .unwrap() else {
            panic!("a call");
        };
        assert_eq!(
            (
                call.on.as_str(),
                call.bench,
                call.device.as_deref(),
                call.label.as_str()
            ),
            ("m", true, Some("gpu:x"), "l")
        );
        assert_eq!(call.command, "echo --on ");
        let (Work::Call(call), sent) =
            named(&["--on", "m", "--key", "k1", "test", "a b", "=", "a b"]).unwrap()
        else {
            panic!("a call");
        };
        assert_eq!(call.command, r"test a\ b = a\ b ");
        assert_eq!(sent.key.as_deref(), Some("k1"));
        assert!(named(&["echo"]).is_err(), "names no machine");
        assert!(named(&["--on", "m"]).is_err(), "runs nothing");
    }
}
