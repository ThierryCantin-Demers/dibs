//! One submission, one wake: a list of dibs command lines run by a driver on this side, with one
//! summary at the end. An agent is woken once for the list rather than once per job, and each
//! wake re-reads its whole context, so this is where most of a busy day's cost goes.
//!
//! The driver is the owner and adds no state on any machine. Each step is the dibs call the
//! agent would have made; if the driver dies its steps die with it and their locks release,
//! which is the lifetime a single job already has. The design is `dibs-design/batch.md`.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: String,
    /// Verbatim, so a failed step is rerun by copying it.
    pub line: String,
    pub after: Vec<String>,
    pub cont: bool,
    pub on: Option<String>,
    pub lock: &'static str,
    pub label: Option<String>,
    pub device: Option<String>,
}

pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut steps: Vec<Step> = Vec::new();
    let mut joined = String::new();
    let mut first_line = 0;
    for (i, raw) in text.lines().enumerate() {
        if joined.is_empty() {
            first_line = i + 1;
        }
        if let Some(head) = raw.strip_suffix('\\') {
            joined.push_str(head);
            joined.push(' ');
            continue;
        }
        joined.push_str(raw);
        let line = std::mem::take(&mut joined);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |e: String| format!("line {first_line}: {e}");
        let (attrs, command) = match line.strip_prefix('[') {
            Some(rest) => {
                let (a, c) = rest.split_once(']').ok_or_else(|| at("an attribute list is not closed with ]".into()))?;
                (a.trim(), c.trim())
            }
            None => ("", line),
        };
        let mut name = None;
        let mut after = None;
        let mut cont = false;
        for attr in attrs.split_whitespace() {
            if attr == "cont" {
                cont = true;
            } else if let Some(v) = attr.strip_prefix("after=") {
                after = Some(v.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>());
            } else if !attr.contains('=') && name.is_none() {
                name = Some(attr.to_string());
            } else {
                return Err(at(format!("unknown attribute '{attr}': a step takes a name, after=a,b and cont")));
            }
        }
        let words = split_words(command).map_err(at)?;
        if words.first().map(String::as_str) != Some("dibs") {
            return Err(at(format!(
                "'{command}' is not a dibs command. A batch is a list of dibs calls, one per line"
            )));
        }
        let s = describe(&words).map_err(at)?;
        let name = name.unwrap_or_else(|| (steps.len() + 1).to_string());
        if steps.iter().any(|p| p.name == name) {
            return Err(at(format!("a second step is named '{name}'")));
        }
        // A step says what it waits for, or waits for the one before it: sequential unless
        // told otherwise, so a list written top to bottom runs top to bottom.
        let after = after.unwrap_or_else(|| steps.last().map(|p| vec![p.name.clone()]).unwrap_or_default());
        steps.push(Step { name, line: command.to_string(), after, cont, ..s });
    }
    if !joined.trim().is_empty() {
        return Err(format!("line {first_line}: the last line ends in a backslash"));
    }
    if steps.is_empty() {
        return Err("the batch has no steps".into());
    }
    let names: HashSet<&str> = steps.iter().map(|s| s.name.as_str()).collect();
    for s in &steps {
        for a in &s.after {
            if !names.contains(a.as_str()) {
                return Err(format!("step '{}' waits for '{a}', and no step has that name", s.name));
            }
        }
    }
    order(&steps)?;
    Ok(steps)
}

/// Fails on a cycle, naming a step in it.
fn order(steps: &[Step]) -> Result<Vec<usize>, String> {
    let index: HashMap<&str, usize> = steps.iter().enumerate().map(|(i, s)| (s.name.as_str(), i)).collect();
    let mut done = vec![false; steps.len()];
    let mut out = Vec::new();
    while out.len() < steps.len() {
        let next = (0..steps.len()).find(|&i| !done[i] && steps[i].after.iter().all(|a| done[index[a.as_str()]]));
        match next {
            Some(i) => {
                done[i] = true;
                out.push(i);
            }
            None => {
                let stuck = (0..steps.len()).find(|&i| !done[i]).unwrap();
                return Err(format!("step '{}' waits on itself through after=", steps[stuck].name));
            }
        }
    }
    Ok(out)
}

/// Shell words without expansion, for reading a step's flags. The step itself runs through bash,
/// so `$DIBS_BATCH` and quoting mean what they mean at a prompt. Anything that would make the
/// line more than one dibs call is refused.
pub fn split_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                        None => return Err("a single quote is not closed".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(x @ ('"' | '\\' | '$' | '`')) => cur.push(x),
                            Some(x) => {
                                cur.push('\\');
                                cur.push(x);
                            }
                            None => return Err("a double quote is not closed".into()),
                        },
                        Some(x) => cur.push(x),
                        None => return Err("a double quote is not closed".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(x) = chars.next() {
                    cur.push(x);
                }
            }
            ';' | '&' | '|' | '<' | '>' | '(' | ')' | '`' => {
                return Err(format!(
                    "'{c}' outside quotes makes this more than one dibs call. Quote the command you are sending"
                ))
            }
            '$' if chars.peek() == Some(&'(') => {
                return Err("$( outside quotes runs a command here, not on the machine. Quote it".into())
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

/// What the driver needs to know about a step before it runs: where it goes, for ordering, and
/// what it is, for the summary. Everything else passes through untouched.
fn describe(words: &[String]) -> Result<Step, String> {
    let mut on = None;
    let mut lock = "shared";
    let mut label = None;
    let mut device = None;
    let mut verb: Option<&str> = None;
    let mut i = 1;
    while i < words.len() {
        let w = words[i].as_str();
        let next = words.get(i + 1).cloned();
        match w {
            "--on" => {
                on = next;
                i += 1;
            }
            "--label" => {
                label = next;
                i += 1;
            }
            "--device" => {
                device = next;
                i += 1;
            }
            "--bench" | "-b" => lock = "bench",
            "--peek" => lock = "peek",
            "--sync" => {
                lock = "sync";
                break;
            }
            "--detach" => return Err("a batch step cannot --detach: the driver owns its steps".into()),
            "--watch" => return Err("a batch step cannot --watch: it never finishes".into()),
            "--" => break,
            _ if w.starts_with('-') => {}
            _ if verb.is_none() => {
                verb = Some(w);
                match w {
                    "batch" => return Err("a batch step cannot be a batch".into()),
                    "build" | "test" | "bench" | "shell" | "raw" => {
                        lock = "recipe";
                        label = Some(words[i..].iter().take(3).filter(|x| !x.starts_with('-')).cloned().collect::<Vec<_>>().join(" "));
                        break;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(Step { name: String::new(), line: String::new(), after: Vec::new(), cont: false, on, lock, label, device })
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Waiting,
    Running,
    Done { exit: i32, seconds: u64 },
    NotRun,
}

/// The steps that may start now, lowest first. A step waits for everything it names, and for
/// its machine to have no other step of this batch on it: two steps on one machine overlapping
/// is the surprise the lock exists to prevent, and nothing is lost by running them in turn.
pub fn ready(steps: &[Step], machines: &[String], states: &[State], stopped: bool) -> Vec<usize> {
    if stopped {
        return Vec::new();
    }
    let index: HashMap<&str, usize> = steps.iter().enumerate().map(|(i, s)| (s.name.as_str(), i)).collect();
    let mut busy: HashSet<&str> = states
        .iter()
        .enumerate()
        .filter(|(_, s)| **s == State::Running)
        .map(|(i, _)| machines[i].as_str())
        .collect();
    let mut out = Vec::new();
    for (i, s) in steps.iter().enumerate() {
        if states[i] != State::Waiting || busy.contains(machines[i].as_str()) {
            continue;
        }
        if s.after.iter().all(|a| matches!(states[index[a.as_str()]], State::Done { .. })) {
            busy.insert(machines[i].as_str());
            out.push(i);
        }
    }
    out
}

/// The job ids a step's trailers named, in order. A recipe step prints several.
pub fn jobs(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("job ").and_then(|r| r.split_whitespace().next()).map(str::to_string))
        .collect()
}

fn duration(s: u64) -> String {
    match s {
        s if s >= 3600 => format!("{}h{:02}m", s / 3600, s / 60 % 60),
        s if s >= 60 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("dibs/batch")
}

/// Where a step goes, asked of dibs itself so the answer is the one the step will reach.
fn machine_of(step: &Step) -> String {
    let mut cmd = Command::new("dibs");
    if let Some(on) = &step.on {
        cmd.args(["--on", on]);
    }
    cmd.arg("--which").stdin(Stdio::null()).stderr(Stdio::null());
    match cmd.output() {
        Ok(o) if o.status.success() => {
            let m = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if m.is_empty() { "?".into() } else { m }
        }
        _ => step.on.clone().unwrap_or_else(|| "?".into()),
    }
}

fn batch_id() -> String {
    let stamp = Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    format!("{stamp}-{}", std::process::id())
}

fn collect_old(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let keep = std::time::Duration::from_secs(14 * 86400);
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > keep);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

pub struct Options {
    pub dry_run: bool,
    pub verbose: bool,
}

pub fn run(text: &str, opts: &Options) -> Result<i32, String> {
    let steps = parse(text)?;
    let machines: Vec<String> = steps.iter().map(machine_of).collect();
    let id = batch_id();
    let plan = plan(&steps, &machines);
    if opts.dry_run {
        print!("batch (dry run), {} steps\n{plan}", steps.len());
        return Ok(0);
    }
    let root = state_dir();
    collect_old(&root);
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    eprint!("dibs: batch {id}, {} steps. You are told when it ends; there is nothing to watch.\n{plan}", steps.len());

    let owned = has("setsid") && has("setpriv");
    let started = Instant::now();
    let mut states = vec![State::Waiting; steps.len()];
    let mut stopped = false;
    let (tx, rx) = mpsc::channel::<(usize, i32, u64)>();
    loop {
        for i in ready(&steps, &machines, &states, stopped) {
            states[i] = State::Running;
            let step = steps[i].clone();
            let (out, err) = (dir.join(format!("{}.out", step.name)), dir.join(format!("{}.err", step.name)));
            let mut cmd = if owned {
                let mut c = Command::new("setsid");
                c.args(["setpriv", "--pdeathsig", "TERM", "bash", "-c", GROUP, "step", &step.line]);
                c
            } else {
                let mut c = Command::new("bash");
                c.args(["-c", &step.line]);
                c
            };
            cmd.env("DIBS_BATCH", &id)
                .env("DIBS_BATCH_STEP", &step.name)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let tx = tx.clone();
            let verbose = opts.verbose;
            std::thread::spawn(move || {
                let t = Instant::now();
                let code = match cmd.spawn() {
                    Ok(mut child) => {
                        let o = copy(child.stdout.take(), out, verbose.then(|| format!("{} ", step.name)));
                        let e = copy(child.stderr.take(), err, verbose.then(|| format!("{} ", step.name)));
                        let status = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                        let _ = (o.join(), e.join());
                        status
                    }
                    Err(_) => 127,
                };
                let _ = tx.send((i, code, t.elapsed().as_secs()));
            });
        }
        if !states.contains(&State::Running) {
            break;
        }
        let (i, exit, seconds) = rx.recv().map_err(|e| e.to_string())?;
        states[i] = State::Done { exit, seconds };
        if exit != 0 && !steps[i].cont {
            stopped = true;
        }
    }
    for s in states.iter_mut() {
        if *s == State::Waiting {
            *s = State::NotRun;
        }
    }
    let report = summary(&id, &steps, &machines, &states, &dir, started.elapsed().as_secs());
    print!("{report}");
    Ok(if states.iter().all(|s| matches!(s, State::Done { exit: 0, .. })) { 0 } else { 1 })
}

fn has(tool: &str) -> bool {
    Command::new(tool).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

/// A step that outlives its driver holds a lock nobody is waiting for. The kernel signals this
/// shell when the driver dies, however it dies, but only this shell: a step running on this
/// machine has a job runner below it holding the lock, so the signal is passed to the step's
/// whole process group, which setsid made its own.
const GROUP: &str = r#"trap 'trap - TERM; kill -TERM 0 2>/dev/null' TERM; bash -c "$1" & wait $!"#;

fn copy(from: Option<impl std::io::Read + Send + 'static>, to: PathBuf, echo: Option<String>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (Some(from), Ok(mut file)) = (from, std::fs::File::create(&to)) else { return };
        for line in BufReader::new(from).lines().map_while(Result::ok) {
            let _ = writeln!(file, "{line}");
            if let Some(prefix) = &echo {
                eprintln!("{prefix}{line}");
            }
        }
    })
}

fn plan(steps: &[Step], machines: &[String]) -> String {
    let w = steps.iter().map(|s| s.name.len()).max().unwrap_or(4).max(4);
    let m = machines.iter().map(String::len).max().unwrap_or(7).max(7);
    let mut s = String::new();
    for (i, st) in steps.iter().enumerate() {
        let after = if st.after.is_empty() { "-".to_string() } else { st.after.join(",") };
        s.push_str(&format!(
            "  {:w$}  {:m$}  {:6}  after {}{}\n",
            st.name,
            machines[i],
            st.lock,
            after,
            if st.cont { ", cont" } else { "" }
        ));
    }
    s
}

pub fn summary(id: &str, steps: &[Step], machines: &[String], states: &[State], dir: &Path, seconds: u64) -> String {
    let failed = states.iter().filter(|s| matches!(s, State::Done { exit, .. } if *exit != 0)).count();
    let not_run = states.iter().filter(|s| **s == State::NotRun).count();
    let mut out = format!("batch {id}  {} steps", steps.len());
    if failed > 0 {
        out.push_str(&format!(", {failed} failed"));
    }
    if not_run > 0 {
        out.push_str(&format!(", {not_run} not run"));
    }
    out.push_str(&format!(", {}\n", duration(seconds)));
    let w = steps.iter().map(|s| s.name.len()).max().unwrap_or(4).max(4);
    let m = machines.iter().map(String::len).max().unwrap_or(7).max(7);
    out.push_str(&format!("{:w$}  {:m$}  {:6}  {:>7}  {:>6}  jobs\n", "name", "machine", "lock", "wall", "exit"));
    for (i, st) in steps.iter().enumerate() {
        let err = std::fs::read_to_string(dir.join(format!("{}.err", st.name))).unwrap_or_default();
        let (wall, exit) = match &states[i] {
            State::Done { exit, seconds } => (duration(*seconds), exit.to_string()),
            _ => ("-".into(), "not run".into()),
        };
        out.push_str(&format!(
            "{:w$}  {:m$}  {:6}  {:>7}  {:>6}  {}\n",
            st.name,
            machines[i],
            st.lock,
            wall,
            exit,
            jobs(&err).join(" ")
        ));
    }
    out.push_str(&format!(
        "each step's output: {}/<name>.out and .err; a job's whole log: dibs --out <job>\n",
        dir.display()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[usize], steps: &[Step]) -> Vec<String> {
        v.iter().map(|&i| steps[i].name.clone()).collect()
    }

    #[test]
    fn a_list_runs_top_to_bottom_unless_a_step_says_what_it_waits_for() {
        let s = parse("dibs --label a 'true'\n\ndibs --label b 'true'\n# a comment\n[c after=] dibs x\n").unwrap();
        assert_eq!(s.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["1", "2", "c"]);
        assert_eq!(s[0].after, Vec::<String>::new());
        assert_eq!(s[1].after, ["1"]);
        assert!(s[2].after.is_empty());
    }

    #[test]
    fn attributes_name_a_step_its_dependencies_and_whether_a_failure_stops_the_rest() {
        let s = parse("[build] dibs --on a --label b 'cargo build'\n[m1 after=build cont] dibs --bench --on a --device gpu:x --label m 'cargo bench'\n").unwrap();
        assert_eq!(s[1].name, "m1");
        assert_eq!(s[1].after, ["build"]);
        assert!(s[1].cont);
        assert_eq!((s[1].on.as_deref(), s[1].lock, s[1].device.as_deref(), s[1].label.as_deref()), (Some("a"), "bench", Some("gpu:x"), Some("m")));
        assert_eq!(s[1].line, "dibs --bench --on a --device gpu:x --label m 'cargo bench'");
    }

    #[test]
    fn a_step_continued_over_lines_is_one_step() {
        let s = parse("[m] dibs --bench --label x \\\n    'cargo bench'\n").unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(split_words(&s[0].line).unwrap(), ["dibs", "--bench", "--label", "x", "cargo bench"]);
    }

    #[test]
    fn what_a_step_is_comes_from_its_own_flags() {
        let one = |l: &str| parse(l).unwrap().remove(0);
        assert_eq!(one("dibs --sync -a ./x :~/y").lock, "sync");
        assert_eq!(one("dibs --peek 'ls'").lock, "peek");
        assert_eq!(one("dibs run --bench 'x'").lock, "bench");
        let r = one("dibs bench cubek@local reduce --device gpu:0");
        assert_eq!((r.lock, r.label.as_deref()), ("recipe", Some("bench cubek@local reduce")));
    }

    #[test]
    fn anything_that_is_not_one_dibs_call_is_refused() {
        for (line, why) in [
            ("cargo build", "not a dibs command"),
            ("dibs 'true' && rm -rf x", "more than one dibs call"),
            ("dibs 'true' | tail", "more than one dibs call"),
            ("dibs $(whoami)", "runs a command here"),
            ("dibs --detach 'x'", "cannot --detach"),
            ("dibs --watch", "cannot --watch"),
            ("dibs batch steps.txt", "cannot be a batch"),
            ("dibs 'unclosed", "not closed"),
            ("[a b=c] dibs x", "unknown attribute"),
        ] {
            let e = parse(line).unwrap_err();
            assert!(e.contains(why), "{line}: {e}");
        }
        assert!(parse("dibs 'a && b; c | d'").is_ok(), "operators inside quotes belong to the remote command");
    }

    #[test]
    fn names_and_dependencies_must_make_sense() {
        assert!(parse("[a] dibs x\n[a] dibs y").unwrap_err().contains("second step is named"));
        assert!(parse("[a after=zz] dibs x").unwrap_err().contains("no step has that name"));
        assert!(parse("[a after=b] dibs x\n[b after=a] dibs y").unwrap_err().contains("waits on itself"));
        assert!(parse("# only a comment\n").unwrap_err().contains("no steps"));
    }

    fn st(n: &str, after: &[&str]) -> Step {
        Step { name: n.into(), line: String::new(), after: after.iter().map(|s| s.to_string()).collect(), cont: false, on: None, lock: "shared", label: None, device: None }
    }

    #[test]
    fn independent_steps_overlap_only_on_different_machines() {
        let steps = [st("build", &[]), st("m1", &["build"]), st("m2", &["build"]), st("m3", &["build"])];
        let machines: Vec<String> = ["x", "x", "y", "x"].iter().map(|s| s.to_string()).collect();
        let mut states = vec![State::Waiting; 4];
        assert_eq!(names(&ready(&steps, &machines, &states, false), &steps), ["build"]);
        states[0] = State::Running;
        assert!(ready(&steps, &machines, &states, false).is_empty());
        states[0] = State::Done { exit: 0, seconds: 1 };
        assert_eq!(names(&ready(&steps, &machines, &states, false), &steps), ["m1", "m2"], "m3 waits for x to be free");
        states[1] = State::Running;
        states[2] = State::Running;
        assert!(ready(&steps, &machines, &states, false).is_empty());
        states[1] = State::Done { exit: 0, seconds: 1 };
        assert_eq!(names(&ready(&steps, &machines, &states, false), &steps), ["m3"]);
        assert!(ready(&steps, &machines, &states, true).is_empty(), "a stopped batch starts nothing");
    }

    #[test]
    fn a_failed_step_still_releases_what_waits_on_it_when_the_batch_goes_on() {
        let steps = [st("a", &[]), st("b", &["a"])];
        let machines = vec!["x".to_string(), "x".to_string()];
        let states = vec![State::Done { exit: 1, seconds: 0 }, State::Waiting];
        assert_eq!(names(&ready(&steps, &machines, &states, false), &steps), ["b"]);
    }

    #[test]
    fn the_job_ids_come_from_the_trailers() {
        let err = "dibs: step 1/2\njob 20260916-1  shared  a:setup  queued 0s  ran 1s  exit 0  by=command\n  log m:/x\njob 20260916-2  bench  a  queued 3s  ran 9s  exit 0  by=command  built=nothing\n";
        assert_eq!(jobs(err), ["20260916-1", "20260916-2"]);
    }
}
