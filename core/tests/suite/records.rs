//! The lines the machine half writes and reads back: lock records, the log, the duration
//! history and a job's directory, each field under the name of what it holds. A runner that
//! replaces the bash half has to write these the same way, and read what bash wrote.

use crate::harness::*;
use crate::recipes::{app, recipes};
use crate::snapshot::*;
use std::fs;
use std::path::Path;

const LOCK_FIELDS: [&str; 9] = ["mode", "pid", "start", "label", "agent", "agent id", "device", "command", "fingerprint"];
const LOG_FIELDS: [&str; 12] = ["when", "event", "pid", "mode", "label", "queued", "ran", "exit", "command", "agent", "batch", "job"];
const HISTORY_FIELDS: [&str; 5] = ["mode", "label", "seconds", "agent", "fingerprint"];

/// One tab-separated line with each field named, and any past the names numbered.
fn fields(line: &str, names: &[&str]) -> String {
    let values: Vec<&str> = line.split('\t').collect();
    let mut out = format!("  {} fields\n", values.len());
    for (i, v) in values.iter().enumerate() {
        let name = names.get(i).map_or_else(|| format!("field {}", i + 1), |n| n.to_string());
        out.push_str(&format!("  {name:<12} {v}\n"));
    }
    out
}

fn records_normal(s: &Sandbox) -> Normal {
    Normal::of(s)
        .rule(r"\b[0-9]+\.any\b", "<pid>.any")
        .rule(r"(after) [0-9]+[hms][0-9hms]*:", "$1 <dur>:")
        .pids()
        .rule(r"(?m)^(  (pid|agent id)\s+)[0-9]+$", "$1<pid>")
        .rule(r"(?m)^(  start\s+)[0-9]{10}$", "$1<epoch>")
        .rule(r"(?m)^(  (queued|ran|seconds)\s+)[0-9]+$", "$1<s>")
        .rule(r"\b(port)\.[0-9]+\b", "$1.<port>")
        .rule(r"(?m)^(  pid\s+)<pid>$", "$1<pid>")
}

/// Every record of one kind in the lock directory, named by its kind with the pid left out.
fn lock_files(s: &Sandbox, kind: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = fs::read_dir(s.lockdir())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{kind}.")))
        .map(|e| {
            let fifo = e.file_type().map(|f| std::os::unix::fs::FileTypeExt::is_fifo(&f)).unwrap_or(false);
            let text = match fifo {
                true => "<a fifo>\n".to_string(),
                false => fs::read_to_string(e.path()).unwrap_or_default(),
            };
            (e.file_name().to_string_lossy().into_owned(), text)
        })
        .collect();
    found.sort();
    found
}

#[test]
fn lock_records() {
    let mut s = Sandbox::new();
    s.machines(&format!("[machine.here]\nssh      = \"here\"\nhostname = \"{}\"\n\n  [[machine.here.device]]\n  kind  = \"cpu\"\n  name  = \"a processor\"\n  cores = 4\n", hostname()));
    let n = records_normal(&s);
    let mut t = Transcript::default();
    let (bench, served, held, up) = (s.gate("bench"), s.gate("served"), s.gate("held"), s.gate("up"));
    let holder = s.spawn(s.dibs(["--on", "here", "--bench", "--device", "cpu", "--label", "rec-bench", &bench.hold()]).env("DIBS_FINGERPRINT", "fp-bench"));
    s.held(1);
    let waiter = s.dibs(["--on", "here", "--label", "rec-served", "--port", "api", "--with", &format!("srv={}", served.hold()), &format!("{}; {}", up.signal(), served.hold())])
        .env("DIBS_BATCH", "20260101-120000-42")
        .env("DIBS_BATCH_STEP", "serve")
        .env("DIBS_BATCH_PLAN", "2\t3\nmeasure\tbench\trec-bench\t1\nelsewhere\tshared\tfar\t0\n");
    let waiter = s.spawn(waiter);
    s.queued(1);
    for kind in ["holder", "waiting"] {
        for (name, text) in lock_files(&s, kind) {
            t.section(&n.apply(&format!("{name}, a benchmark holding and a shared job queued behind it")), &n.apply(&fields(text.trim_end_matches('\n'), &LOCK_FIELDS)));
        }
    }
    for (name, text) in lock_files(&s, "batch") {
        t.section(&n.apply(&format!("{name}, the plan of the batch the queued job is a step of")), &n.apply(&text));
    }
    bench.open();
    s.wait(holder);
    s.held(1);
    up.reached();
    for kind in ["holder", "with", "port", "work"] {
        for (name, text) in lock_files(&s, kind) {
            let text = text.trim_end_matches('\n');
            let body = match kind {
                "holder" => fields(text, &LOCK_FIELDS),
                "with" => fields(text, &["service", "pid", "command"]),
                "port" => fields(text, &["pid"]),
                _ => format!("  {} lines, a pid each\n", text.lines().count()),
            };
            t.section(&n.apply(&format!("{name}, once the queued job holds the lock")), &n.apply(&body));
        }
    }
    s.status();
    for (name, text) in lock_files(&s, "cpu") {
        let sample: Vec<&str> = text.split_whitespace().collect();
        t.section(&n.apply(&format!("{name}, the CPU sample a status leaves")), &format!("  {} words: ticks, when it last worked, when sampled\n", sample.len()));
    }
    served.open();
    s.wait(waiter);
    let hold = s.spawn(s.dibs(["--on", "here", "--hold", "--label", "rec-hold", &held.hold()]));
    s.held(1);
    for (name, text) in lock_files(&s, "holder") {
        t.section(&n.apply(&format!("{name}, a hold")), &n.apply(&fields(text.trim_end_matches('\n'), &LOCK_FIELDS)));
    }
    for (name, text) in lock_files(&s, "hold") {
        t.section(&n.apply(&format!("{name}, beside the hold's record")), &text);
    }
    held.open();
    s.wait(hold);
    s.gone();
    let left: Vec<String> = fs::read_dir(s.lockdir()).unwrap().flatten().map(|e| n.apply(&e.file_name().to_string_lossy())).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    t.section("what the lock directory holds once every job has gone", &format!("{}\n", left.join("\n")));
    snapshot("records-lock", t.text());
}

#[test]
fn log_and_history() {
    let mut s = Sandbox::new();
    s.history(&"shared\tquickie\t1\tsomeone\n".repeat(3));
    let n = records_normal(&s);
    s.dibs(["--label", "rec-plain", "echo hi"]).env("DIBS_FINGERPRINT", "fp-plain").run();
    s.dibs(["--label", "rec-fails", "exit 3"]).run();
    s.dibs(["--peek", "true"]).env("DIBS_PEEK_WARN", "0").run();

    let (b, k, q) = (s.gate("b"), s.gate("k"), s.gate("q"));
    let anchor = s.spawn(s.dibs(["--label", "rec-anchor", &b.hold()]));
    s.held(1);
    let bench = s.spawn(s.dibs(["--bench", "--label", "rec-queued-bench", "true"]));
    s.queued(1);
    let quick = s.spawn(s.dibs(["--label", "quickie", &q.hold()]).env("DIBS_PATIENCE", "600"));
    s.held(2);
    q.open();
    s.wait(quick);
    b.open();
    s.wait(anchor);
    s.wait(bench);

    let doomed = s.spawn(s.dibs(["--label", "rec-killed", &k.hold()]).session("owner"));
    s.held(1);
    let pid = s.pid_of("rec-killed").to_string();
    s.dibs(["--kill", &pid, "--anyone"]).session("stranger").run();
    s.wait(doomed);
    s.gone();

    let (up, never) = (s.gate("up"), s.gate("never"));
    let caller = s.spawn(s.remote(s.dibs(["--label", "rec-gone", &format!("{}; {}", up.signal(), never.hold())])));
    up.reached();
    unsafe { libc::kill(caller.pid as i32, libc::SIGKILL) };
    s.wait(caller);
    s.log_line("caller-gone.*rec-gone");
    s.gone();

    let id = "20260101-120000-42";
    let (sup, shold) = (s.gate("sup"), s.gate("shold"));
    let step = s.spawn(s.dibs(["--label", "rec-step", &format!("{}; {}", sup.signal(), shold.hold())]).env("DIBS_BATCH", id).env("DIBS_BATCH_STEP", "first"));
    sup.reached();
    s.dibs(["--kill", id]).env("DIBS_KILL_HERE", "1").run();
    s.wait(step);
    s.dibs(["--label", "rec-late", "true"]).env("DIBS_BATCH", id).env("DIBS_BATCH_STEP", "late").run();
    let cancelled: Vec<String> = lock_files(&s, "cancelled").into_iter().map(|(name, text)| format!("{name}: {} bytes", text.len())).collect();

    let mut t = Transcript::default();
    let log = s.log();
    for line in log.lines() {
        let event = line.split('\t').nth(1).unwrap_or("?");
        let label = line.split('\t').nth(4).unwrap_or("?");
        t.section(&n.apply(&format!("log: {event}, {label}")), &n.apply(&fields(line, &LOG_FIELDS)));
    }
    for line in s.read("history").lines() {
        t.section(&format!("history: {}", line.split('\t').nth(1).unwrap_or("?")), &n.apply(&fields(line, &HISTORY_FIELDS)));
    }
    t.section("cancelled.<batch> in the lock directory", &format!("{}\n", n.apply(&cancelled.join("\n"))));
    snapshot("records-log", t.text());
}

#[test]
fn orphan_reclaimed_in_the_log() {
    if cfg!(target_os = "macos") {
        eprintln!("skipped on macOS: no process there can be shown to hold an flock, so no orphan is ever reclaimed");
        return;
    }
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let script = "exec 8>\"$1/rw\"; flock -x 8; printf 'up\\n' > \"$2\"; read -r _ < \"$3\"";
    let lockdir = s.var("DIBS_LOCK_DIR");
    let call = s.new_session(["bash", "-c", script, "_", &lockdir, &up.path.display().to_string(), &never.path.display().to_string()]);
    let orphan = s.spawn(call);
    up.reached();
    s.dibs(["--release"]).run();
    s.wait(orphan);
    let n = records_normal(&s);
    let line = s.log().lines().find(|l| l.contains("\treclaimed\t")).unwrap_or_default().to_string();
    let mut t = Transcript::default();
    t.section("log: reclaimed", &n.apply(&fields(&line, &LOG_FIELDS)));
    snapshot("records-reclaimed", t.text());
}

fn job_dirs(scratch: &Path) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<_> = fs::read_dir(scratch.join("jobs")).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    dirs.sort();
    dirs
}

/// A job's directory: every file under it, what each holds unless `files_only`, and its meta field
/// by field.
fn job_directory(n: &Normal, dir: &Path, files_only: bool) -> String {
    let mut out = String::new();
    let mut files: Vec<_> = walk(dir);
    files.sort();
    for f in files {
        let rel = f.strip_prefix(dir).unwrap().display().to_string();
        let text = fs::read_to_string(&f).unwrap_or_default();
        out.push_str(&format!("  {rel}\n"));
        if rel == "meta" {
            for line in text.lines() {
                out.push_str(&format!("    {}\n", line.replacen('\t', " = ", 1)));
            }
        } else if !files_only && !rel.starts_with("artifacts/") {
            out.extend(text.lines().map(|l| format!("    | {l}\n")));
        }
    }
    n.apply(&out)
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn job_directories() {
    let s = Sandbox::new();
    let n = records_normal(&s).rule(r"(?m)^(    (queued|ran) = )[0-9]+$", "$1<s>");
    let mut t = Transcript::default();
    let scratch = s.path("scratch");
    let runs: [(&str, &[&str]); 4] = [
        ("a job that printed to both streams", &["--label", "rec-plain", "echo out; echo err >&2"]),
        ("a job that failed", &["--label", "rec-fails", "exit 3"]),
        ("a job with a service", &["--label", "rec-with", "--with", "srv=echo serving", "true"]),
        ("a benchmark that compiled nothing", &["--bench", "--label", "rec-bench", "echo '    Finished `release` profile [optimized] target(s) in 0.01s'"]),
    ];
    for (what, args) in runs {
        let before = job_dirs(&scratch);
        s.dibs(args).run();
        let new: Vec<_> = job_dirs(&scratch).into_iter().filter(|d| !before.contains(d)).collect();
        assert_eq!(new.len(), 1, "{what} made one directory");
        t.section(&format!("jobs/<job>, {what}: dibs {}", typed(args)), &job_directory(&n, &new[0], false));
    }
    let dir = app(&s);
    recipes(&s, "[bench.kept]\nartifacts = [\"out/*.json\"]\n  [[bench.kept.step]]\n  lock = \"exclusive\"\n  run = \"mkdir -p out && echo '{}' > out/r.json\"\n");
    let before = job_dirs(&scratch);
    s.dibs(["bench", &format!("{dir}@local"), "kept"]).run();
    let new: Vec<_> = job_dirs(&scratch).into_iter().filter(|d| !before.contains(d)).collect();
    for d in new {
        t.section("jobs/<job>, a recipe's job, with the files it kept", &job_directory(&n, &d, true));
    }
    snapshot("records-job", t.text());
}

#[test]
fn what_this_computer_keeps() {
    let mut s = Sandbox::new();
    s.machines(&format!("[machine.here]\nssh      = \"here\"\nhostname = \"{}\"\n", hostname()));
    let n = records_normal(&s)
        .rule(r"\b1[0-9]{9}\b", "<epoch>")
        .rule(r"\b[0-9a-f]{12}\b", "<sha>")
        .rule(r"\b[0-9a-f]{40}\b", "<commit>")
        .rule(r"local-[0-9a-f]{10}\b", "local-<key>")
        .rule(r"local:[0-9a-f]{7,}(\+dirty)?-", "local:<head>$1-")
        .rule(r#""dibs":"[0-9a-f]{7,12}""#, r#""dibs":"<commit>""#)
        .rule(r#""seconds": [0-9]+"#, r#""seconds": <s>"#)
        .rule(r"ran [0-9]+s  exit", "ran <s>  exit");
    let mut t = Transcript::default();
    s.dibs(["--on", "here", "--bench", "--label", "rec-series", "true"]).run();
    t.section("series", &n.apply(&s.read("series")));
    let job = job_id(&s.dibs(["--label", "rec-kept", "seq 1 3"]).run().stderr);
    s.dibs(["out", &job]).run();
    let kept = s.path(&format!("home/.local/state/dibs/jobs/{job}"));
    t.section("jobs/<job> kept on this computer once read", &job_directory(&n, &kept, false));
    s.dibs(["--friction", "a line about what got in the way"]).run();
    t.section("friction.jsonl", &n.apply(&s.read("home/.local/state/dibs/friction.jsonl")));
    let dir = app(&s);
    recipes(&s, "[build.rec]\n  [[build.rec.step]]\n  lock = \"shared\"\n  run = \"true\"\n");
    s.dibs(["build", &format!("{dir}@local"), "rec"]).run();
    t.section("runs.jsonl, one recipe run, as JSON", &n.apply(&json_lines(&s.read("home/.local/state/dibs/runs.jsonl"))));
    s.dibs(["batch", "-"]).stdin("[only] dibs --label rec-batch true\n").run();
    let batches = s.path("home/.local/state/dibs/batch");
    let one = fs::read_dir(&batches).unwrap().flatten().next().unwrap().path();
    let mut listing: Vec<String> = walk(&one).iter().map(|f| f.strip_prefix(&one).unwrap().display().to_string()).collect();
    listing.sort();
    t.section("batch/<batch>, what a batch keeps of its steps", &format!("{}\n", listing.join("\n")));
    snapshot("records-client", t.text());
}
