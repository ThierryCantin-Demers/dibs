//! Everything an agent reads from dibs, pinned word for word, so that a rewrite of either half
//! can be held to printing the same thing. Times, ids, pids and paths are normalised; everything
//! else is as printed.

use crate::harness::*;
use crate::recipes::{PARAMS, app, fake_cargo, recipes};
use crate::snapshot::*;
use std::fs;

/// The durations a holder's clock or the queue's decides, with what history says left alone.
fn status_clock(n: Normal) -> Normal {
    n.clocked()
        .rule(
            r"(?m)^(  [a-z]+  \S+  )[0-9]+[hms][0-9hms]*(  pid )",
            "$1<dur>$2",
        )
        .rule(r"(~|under )([0-9]+[hms][0-9hms]*) left", "$1{$2} left")
        .rule(r"waiting [0-9]+[hms][0-9hms]*", "waiting <dur>")
        .rule(
            r"~([0-9]+[hms][0-9hms]*) until it starts",
            "~{$1} until it starts",
        )
        .rule(
            r"batch time left here: (over |~)([0-9]+[hms][0-9hms]*)",
            "batch time left here: $1{$2}",
        )
        .rule(
            r"IDLE: no CPU at all in [0-9hms]+",
            "IDLE: no CPU at all in <dur>",
        )
        .rule(
            r"IDLE: [0-9]+s of CPU, none of it in the last [0-9hms]+",
            "IDLE: <n>s of CPU, none of it in the last <dur>",
        )
        .rule(r"(?m)^[0-9]{2}:[0-9]{2}:[0-9]{2}(   every)", "<clock>$1")
        .pids()
}

/// The clocks and the machine's own numbers normalised, the durations pinned.
fn json_clock(n: Normal) -> Normal {
    n.rule(
        r#""(t|cores|load|pid|started|cpu|arrived)": -?[0-9]+"#,
        r#""$1": <n>"#,
    )
    .rule(
        r#""(elapsed|cpu_rate|remaining|waiting|eta|left|idle_for)": ([0-9]+)"#,
        r#""$1": {$2}"#,
    )
}

/// Each call in turn, under the arguments it was given.
fn transcript_of(s: &Sandbox, n: &Normal, calls: &[&[&str]]) -> String {
    let mut t = Transcript::default();
    for args in calls {
        t.section(
            &format!("dibs {}", typed(args)),
            &n.output(&s.dibs(*args).run()),
        );
    }
    t.text().to_string()
}

#[test]
fn help() {
    let s = Sandbox::new();
    let n = Normal::of(&s);
    snapshot("help", &transcript_of(&s, &n, &[&["--help"]]));
    assert_eq!(
        s.dibs(["-h"]).run().stdout,
        s.dibs(["--help"]).run().stdout,
        "-h is --help"
    );
    snapshot(
        "help-recipes",
        &transcript_of(&s, &n, &[&["build", "--help"], &["build"]]),
    );
    assert_eq!(
        s.dibs(["list", "-h"]).run().stdout,
        s.dibs(["build", "--help"]).run().stdout,
        "every verb gives one help"
    );
}

#[test]
fn trailer() {
    let s = Sandbox::new();
    s.history(&"shared\tlong-suite\t1500\tx\n".repeat(3));
    let never = s.gate("never");
    let n = Normal::of(&s)
        .clocked()
        .pids()
        .rule(r"(port api): [0-9]+ on", "$1: <port> on");
    let finished = "echo '    Finished `release` profile [optimized] target(s) in 0.24s'";
    let built = format!("echo '   Compiling a v1'; echo '   Compiling b v1'; {finished}");
    let held = format!("read -r _ <> {}", never.path.display());
    let service = format!("srv={held}");
    let mut t = Transcript::default();
    let calls: &[&[&str]] = &[
        &["--label", "plain", "echo hello; echo err >&2"],
        &["--label", "fails", "echo out; exit 4"],
        &["--label", "fails", "echo out; exit 4"],
        &["--label", "digest", "seq 1 300"],
        &["--stream", "--label", "streamed", "seq 1 3"],
        &["--label", "built", &built],
        &["--label", "built-nothing", finished],
        &["--label", "long-suite", "true"],
        &["--bench", "--label", "a-bench", "true"],
        &["--max", "1", "--label", "overran", &held],
        &["--hold", "--label", "held-here", "echo from here"],
        &["--hold", "--max", "1", "--label", "held-over", &held],
        &[
            "--label",
            "served",
            "--port",
            "api",
            "--with",
            &service,
            "--ready",
            "true",
            "test -n \"$DIBS_PORT_API\" && echo the port reached the command",
        ],
        &[
            "--label",
            "bad-service",
            "--with",
            "bad=echo oops; exit 4",
            "--ready",
            "false",
            "true",
        ],
        &[
            "--label",
            "slow-service",
            "--with",
            &service,
            "--ready",
            "false",
            "--ready-within",
            "1",
            "true",
        ],
        &["--label", "service-dies", "--with", "brief=exit 5", &held],
    ];
    for args in calls {
        t.section(
            &n.apply(&format!("dibs {}", typed(args))),
            &n.output(&s.dibs(*args).run()),
        );
    }
    let peek = s
        .dibs([
            "--peek",
            "timeout 1.5 python3 -c 'while True: pass'; echo peeked",
        ])
        .env("DIBS_PEEK_WARN", "1")
        .run();
    t.section(
        "DIBS_PEEK_WARN=1 dibs --peek '<a second and a half of work>'",
        &n.output(&peek),
    );
    t.section(
        "dibs --peek 'echo cheap'",
        &n.output(&s.dibs(["--peek", "echo cheap"]).run()),
    );
    snapshot("trailer", t.text());
}

/// One lock record as the machine half writes it: nine tab-separated fields.
#[derive(Clone, Copy)]
struct Rec<'a> {
    mode: &'a str,
    label: &'a str,
    /// Seconds since it arrived, or since it acquired for a holder.
    age: u64,
    agent: &'a str,
    owner: &'a str,
    device: &'a str,
    cmd: &'a str,
    fingerprint: &'a str,
}

impl Default for Rec<'_> {
    fn default() -> Self {
        Rec {
            mode: "shared",
            label: "a-label",
            age: 0,
            agent: "an agent",
            owner: "local_owner",
            device: "-",
            cmd: "the command",
            fingerprint: "",
        }
    }
}

impl Rec<'_> {
    fn write(&self, s: &Sandbox, kind: &str, pid: u32) {
        let (pid_s, start) = (pid.to_string(), (now() - self.age).to_string());
        s.record(
            kind,
            pid,
            &[
                self.mode,
                &pid_s,
                &start,
                self.label,
                self.agent,
                self.owner,
                self.device,
                self.cmd,
                self.fingerprint,
            ],
        );
    }
}

/// Live idle processes for records to name, since prune drops a record whose process has gone.
fn idlers(s: &mut Sandbox, gate: &Gate, count: usize) -> Vec<u32> {
    let mut pids: Vec<u32> = (0..count)
        .map(|_| {
            let call = s.command("cat", [gate.path.display().to_string()]);
            s.spawn(call).pid
        })
        .collect();
    pids.sort_by_key(|p| p.to_string());
    pids
}

fn without_samples(s: &Sandbox) {
    for e in fs::read_dir(s.lockdir()).unwrap().flatten() {
        if e.file_name().to_string_lossy().starts_with("cpu.") {
            let _ = fs::remove_file(e.path());
        }
    }
}

fn forget_records(s: &Sandbox) {
    for e in fs::read_dir(s.lockdir()).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if ["holder.", "waiting.", "with.", "batch.", "cpu."]
            .iter()
            .any(|k| name.starts_with(k))
        {
            let _ = fs::remove_file(e.path());
        }
    }
}

const STATUS_HISTORY: &str = "\
bench\tsteady\t100\tan agent
bench\tsteady\t100\tan agent
bench\tsteady\t100\tan agent
bench\tnext-bench\t300\tan agent
bench\tnext-bench\t300\tan agent
bench\tnext-bench\t300\tan agent
shared\tmixed\t0\tan agent
shared\tmixed\t0\tan agent
shared\tmixed\t0\tan agent
shared\tmixed\t200\tan agent
shared\tmixed\t240\tan agent
shared\tsolo\t120\tan agent
shared\tquick\t0\tan agent
shared\tquick\t0\tan agent
shared\tquick\t0\tan agent
shared\tother-work\t20\tAgent One
shared\tother-work\t20\tAgent One
shared\tother-work\t20\tAgent One
shared\tsweep\t20\tan agent\tsmall
shared\tsweep\t20\tan agent\tsmall
shared\tsweep\t20\tan agent\tsmall
shared\tstuck-one\t20\tan agent
shared\tstuck-one\t20\tan agent
shared\tstuck-one\t20\tan agent
shared\tbatch-next\t300\tan agent
shared\tbatch-next\t300\tan agent
shared\tbatch-next\t300\tan agent
";

/// `dibs status` and `dibs status --json` of one lock directory, as sections of two snapshots.
struct Looks {
    text: Transcript,
    json: Transcript,
    n: Normal,
    jn: Normal,
}

impl Looks {
    fn new(s: &Sandbox) -> Looks {
        let n = status_clock(Normal::of(s));
        Looks {
            text: Transcript::default(),
            json: Transcript::default(),
            jn: json_clock(n.clone()),
            n,
        }
    }

    /// Each from a first look, since what a look leaves behind changes what the next one says.
    fn look(&mut self, s: &Sandbox, title: &str) {
        without_samples(s);
        self.text
            .section(title, &self.n.output(&s.dibs(["status"]).run()));
        without_samples(s);
        self.json.section(
            title,
            &self
                .jn
                .apply(&json_lines(&s.dibs(["status", "--json"]).run().stdout)),
        );
    }
}

#[test]
fn status() {
    let mut s = Sandbox::new();
    s.set_history(STATUS_HISTORY);
    s.set("DIBS_IDLE_AFTER", "1000000");
    let gate = s.gate("idlers");
    let mut looks = Looks::new(&s);
    looks.look(&s, "idle");

    let [bench, q1, q2] = idlers(&mut s, &gate, 3)[..] else {
        unreachable!()
    };
    Rec {
        mode: "bench",
        label: "steady",
        age: 30,
        device: "gpu:card",
        cmd: "the steady job",
        ..Rec::default()
    }
    .write(&s, "holder", bench);
    s.write(
        &format!("lockdir/with.{bench}"),
        "srv\t4242\t./serve --port 1\n",
    );
    s.write(&format!("lockdir/batch.{bench}"), "20260101-120000-42\tbuild\n1\t4\nnext\tshared\tbatch-next\t1\nfresh\tshared\tnever-ran\t1\nfar\tshared\tbatch-far\t0\n");
    Rec {
        label: "behind-1",
        age: 2,
        cmd: "the first waiter",
        ..Rec::default()
    }
    .write(&s, "waiting", q1);
    Rec {
        mode: "bench",
        label: "next-bench",
        age: 1,
        cmd: "the second waiter",
        ..Rec::default()
    }
    .write(&s, "waiting", q2);
    looks.look(
        &s,
        "a benchmark holding, with a service and a batch plan, and two queued",
    );
    forget_records(&s);

    let shared = [
        Rec {
            label: "mixed",
            age: 30,
            cmd: "runs disagree",
            ..Rec::default()
        },
        Rec {
            label: "solo",
            age: 30,
            cmd: "ran once before",
            ..Rec::default()
        },
        Rec {
            label: "quick",
            age: 30,
            cmd: "always instant",
            ..Rec::default()
        },
        Rec {
            label: "novel",
            age: 30,
            agent: "Agent One",
            cmd: "an agent's new label",
            ..Rec::default()
        },
        Rec {
            label: "never-seen",
            age: 30,
            agent: "Agent Three",
            cmd: "nothing known",
            ..Rec::default()
        },
        Rec {
            label: "sweep",
            age: 600,
            cmd: "other values",
            fingerprint: "big",
            ..Rec::default()
        },
        Rec {
            label: "stuck-one",
            age: 600,
            cmd: "far past its slowest",
            ..Rec::default()
        },
        Rec {
            mode: "rsh",
            label: "sync",
            age: 5,
            cmd: "rsync --server",
            ..Rec::default()
        },
    ];
    let pids = idlers(&mut s, &gate, shared.len() + 1);
    for (r, pid) in shared.iter().zip(&pids) {
        r.write(&s, "holder", *pid);
    }
    let old = pids[shared.len()];
    let (pid, start) = (old.to_string(), (now() - 40).to_string());
    s.record(
        "holder",
        old,
        &[
            "shared",
            &pid,
            &start,
            "six-fields",
            "an older dibs",
            "its command",
        ],
    );
    looks.look(
        &s,
        "shared holders, each estimated from a different history",
    );
    forget_records(&s);
    snapshot("status", looks.text.text());
    snapshot("status-json", looks.json.text());
    gate.open();
}

#[test]
fn status_of_idle_and_writing_holders() {
    let mut s = Sandbox::new();
    s.set("DIBS_IDLE_AFTER", "-1");
    let gate = s.gate("idlers");
    let burned = s.gate("burned");
    let [never] = idlers(&mut s, &gate, 1)[..] else {
        unreachable!()
    };
    let burner = format!(
        "import time\ns = time.process_time()\nwhile time.process_time() - s < 0.3: pass\nopen('{}', 'w').write('up')\nopen('{}').read()",
        burned.path.display(),
        gate.path.display()
    );
    let worked = s.spawn(s.command("python3", ["-c", &burner])).pid;
    burned.reached();
    let writes = s
        .spawn(s.command(
            "sh",
            [
                "-c",
                &format!("cat {} > {}", gate.path.display(), s.p("written.log")),
            ],
        ))
        .pid;
    until("the writer's redirect", || s.exists("written.log"));
    let n = status_clock(Normal::of(&s));
    let mut t = Transcript::default();
    for (pid, label, age) in [
        (never, "never-worked", 600),
        (worked, "worked-once", 30),
        (writes, "writes-a-log", 30),
    ] {
        Rec {
            label,
            age,
            cmd: label,
            ..Rec::default()
        }
        .write(&s, "holder", pid);
        t.section(
            &format!("dibs status  ({label}, a first look with nothing to compare against)"),
            &n.output(&s.dibs(["status"]).run()),
        );
        t.section(
            &format!("dibs status  ({label}, the next look)"),
            &n.output(&s.dibs(["status"]).run()),
        );
        forget_records(&s);
    }
    snapshot("status-idle", t.text());
    gate.open();
}

#[test]
fn status_with_the_lock_taken_and_nothing_recorded() {
    // The runner reads who holds the lock from the kernel, so stubs of the tools a bash machine
    // half asked change nothing: a holder in the caller's own group is a client recording itself.
    let mut s = Sandbox::new();
    for tool in ["fuser", "lsof"] {
        s.write_exec(&format!("bin/{tool}"), "#!/bin/sh\nexit 1\n");
    }
    let (up, release) = (s.gate("up"), s.gate("release"));
    let script = format!(
        "exec 8>\"$1/rw\"; flock -s 8; echo up > {}; read -r _ < {}",
        up.path.display(),
        release.path.display()
    );
    let holder = s.spawn(s.command("bash", ["-c", &script, "_", &s.var("DIBS_LOCK_DIR")]));
    up.reached();
    let n = status_clock(Normal::of(&s));
    let mut t = Transcript::default();
    t.section("dibs status", &n.output(&s.dibs(["status"]).run()));
    t.section(
        "dibs status --json",
        &json_clock(n.clone()).apply(&json_lines(&s.dibs(["status", "--json"]).run().stdout)),
    );
    t.section(
        "dibs --bench --wait 1 --label behind-it true",
        &n.output(
            &s.dibs(["--bench", "--wait", "1", "--label", "behind-it", "true"])
                .run(),
        ),
    );
    release.open();
    s.wait(holder);
    let script = "exec 8>\"$1/rw\"; flock -s 8
        printf \"shared\\t%s\\t%s\\tinq\\tsomeone\\tid\\t-\\ttrue\\n\" \"$$\" \"$(date +%s)\" > \"$1/waiting.$$\"
        \"$2\" --status 2>&1; rm -f \"$1/waiting.$$\"";
    let unstubbed = s.var("PATH").replacen(
        &format!("{}:", s.p("bin")),
        &format!("{}:", s.p("dibs-only")),
        1,
    );
    fs::create_dir_all(s.path("dibs-only")).unwrap();
    std::os::unix::fs::symlink(DIBS, s.path("dibs-only/dibs")).unwrap();
    let recording = s
        .command("bash", ["-c", script, "_", &s.var("DIBS_LOCK_DIR"), DIBS])
        .env("PATH", unstubbed)
        .run();
    t.section(
        "dibs status, asked by a client that has just taken the lock and not yet recorded it",
        &n.output(&recording),
    );
    snapshot("status-unrecorded", t.text());
}

/// Holds the lock from a session of its own, as the remains of one that has gone would.
fn orphan(s: &mut Sandbox, up: &Gate, release: &Gate) -> Job {
    let script = "exec 8>\"$1/rw\"; flock -x 8; printf 'up\\n' > \"$2\"; read -r _ < \"$3\"";
    let lockdir = s.var("DIBS_LOCK_DIR");
    let call = s.new_session([
        "bash",
        "-c",
        script,
        "_",
        &lockdir,
        &up.path.display().to_string(),
        &release.path.display().to_string(),
    ]);
    s.spawn(call)
}

#[test]
fn status_of_an_orphaned_lock() {
    if cfg!(target_os = "macos") {
        skip(
            "on macOS, no process there can be shown to hold an flock, so no orphan is ever named",
        );
        return;
    }
    let mut s = Sandbox::new();
    let (up, release) = (s.gate("up"), s.gate("release"));
    let o = orphan(&mut s, &up, &release);
    up.reached();
    let n = status_clock(Normal::of(&s)).rule(
        r"(?m)^( +)[0-9]+ +[0-9:-]+ +\S+ .*$",
        "$1<pid> <etime> <user> <args>",
    );
    let mut t = Transcript::default();
    t.section("dibs status", &n.output(&s.dibs(["status"]).run()));
    t.section(
        "dibs status --json",
        &json_clock(n.clone()).apply(&json_lines(&s.dibs(["status", "--json"]).run().stdout)),
    );
    t.section(
        "dibs --wait 1 --label behind-it true",
        &n.output(
            &s.dibs(["--wait", "1", "--label", "behind-it", "true"])
                .run(),
        ),
    );
    t.section("dibs --release", &n.output(&s.dibs(["--release"]).run()));
    snapshot("status-orphan", t.text());
    s.wait(o);
}

#[test]
fn watch() {
    let mut s = Sandbox::new();
    let n = status_clock(Normal::of(&s));
    let mut first_tick = |args: &[&str], until_line: &str| {
        let file = s.path("watch.out");
        let job = s.spawn(s.dibs(args).stdout_to(&file));
        let mut seen = String::new();
        until("the first redraw", || {
            seen = fs::read_to_string(&file).unwrap_or_default();
            seen.contains(until_line)
        });
        unsafe { libc::kill(job.pid as i32, libc::SIGKILL) };
        s.wait(job);
        let end = seen.find(until_line).unwrap() + until_line.len();
        n.apply(&seen[..end])
    };
    let mut t = Transcript::default();
    t.section(
        "dibs --watch 2, its first redraw",
        &first_tick(&["--watch", "2"], "dibs: idle\n"),
    );
    t.section(
        "dibs --watch --json, its first line",
        &json_clock(n.clone()).apply(&json_lines(&first_tick(&["--watch", "--json"], "\n"))),
    );
    snapshot("watch", t.text());
}

#[test]
fn queued_line() {
    let mut s = Sandbox::new();
    s.history(&"bench\tlong-bench\t600\tan agent\n".repeat(3));
    s.history(&"shared\tquick\t1\tan agent\n".repeat(3));
    s.history(&"shared\tbuild\t60\tan agent\n".repeat(3));
    let n = status_clock(Normal::of(&s));
    let mut t = Transcript::default();
    let mut tell = |s: &Sandbox, args: &[&str], what: &str| {
        t.section(
            &format!("dibs {}  ({what})", typed(args)),
            &n.output(&s.dibs(args).run()),
        );
    };
    let gates: Vec<Gate> = ["h1", "h2", "s1"].iter().map(|g| s.gate(g)).collect();
    let holder = s.spawn(s.dibs(["--bench", "--label", "no-history", &gates[0].hold()]));
    s.held(1);
    tell(
        &s,
        &["--wait", "1", "--label", "queued", "true"],
        "behind a benchmark with no history of its own",
    );
    gates[0].open();
    s.wait(holder);

    let shared: Vec<Job> = (0..2)
        .map(|_| s.spawn(s.dibs(["--label", "build", &gates[2].hold()])))
        .collect();
    s.held(2);
    tell(
        &s,
        &["--bench", "--wait", "1", "--label", "a-bench", "true"],
        "a benchmark behind two shared holders",
    );
    gates[2].open();
    for j in shared {
        s.wait(j);
    }

    let holder = s.spawn(s.dibs(["--bench", "--label", "long-bench", &gates[1].hold()]));
    s.held(1);
    let bench = s.spawn(s.dibs(["--bench", "--label", "queued-bench", "true"]));
    s.queued(1);
    tell(
        &s,
        &["--wait", "1", "--label", "quick", "true"],
        "a quick job going around a queued benchmark, behind the one running",
    );
    tell(
        &s,
        &["--wait", "1", "--label", "gated", "true"],
        "with a benchmark queued ahead, holding the gate",
    );
    gates[1].open();
    for j in [holder, bench] {
        s.wait(j);
    }
    snapshot("queued", t.text());
}

#[test]
fn acquired_after_waiting() {
    let mut s = Sandbox::new();
    let n = status_clock(Normal::of(&s));
    let ends = s.gate("ends");
    let slow = s.spawn(s.dibs(["--bench", "--label", "six-seconds", &ends.hold()]));
    s.held(1);
    let args = ["--label", "waits-it-out", "true"];
    let (out, err) = (s.path("waiter.out"), s.path("waiter.err"));
    let waiter = s.spawn(s.dibs(args).streams_to(&out, &err));
    s.queued(1);
    // Only a wait of five seconds or more is said, so the benchmark ends six after it arrived.
    let arrived: u64 = s.records("waiting")[0][2].parse().unwrap();
    until("the waiter to have waited six seconds", || {
        now() >= arrived + 6
    });
    ends.open();
    s.wait(slow);
    let code = s.wait(waiter);
    let said = Output {
        code,
        stdout: fs::read_to_string(&out).unwrap_or_default(),
        stderr: fs::read_to_string(&err).unwrap_or_default(),
    };
    let mut t = Transcript::default();
    t.section(
        &format!(
            "dibs {}  (behind a benchmark that ends six seconds after it arrives)",
            typed(&args)
        ),
        &n.output(&said),
    );
    snapshot("acquired", t.text());
}

fn batch_file(s: &Sandbox, name: &str, lines: &[&str]) -> String {
    s.write(name, &format!("{}\n", lines.join("\n")));
    s.p(name)
}

#[test]
fn batch_summary() {
    let mut s = Sandbox::new();
    let finished = "echo \"    Finished \\`release\\` profile [optimized] target(s) in 0.01s\"";
    let n = Normal::of(&s)
        .clocked()
        .rule(
            r"(?m)^(batch <batch>  .*, )[0-9]+[hms][0-9hms]*$",
            "$1<dur>",
        )
        .rule(
            r"(?m)^(\S+ +\S+ +[a-z]+ +)[0-9]+[hms][0-9hms]*( )",
            "$1<dur>$2",
        );
    let file = batch_file(
        &s,
        "steps",
        &[
            "# a comment",
            "[build] dibs --label batch-build 'echo building'",
            &format!("[measure after=build] dibs --bench --label batch-measure '{finished}'"),
            "[sweep-a cont] dibs --label batch-sweep 'exit 3'",
            "[sweep-b] dibs --label batch-sweep 'exit 4'",
            "[never] dibs --label batch-never true",
        ],
    );
    let mut t = Transcript::default();
    t.section(
        "dibs batch --dry-run steps",
        &n.output(&s.dibs(["batch", "--dry-run", &file]).run()),
    );
    t.section(
        "dibs batch steps",
        &n.output(&s.dibs(["batch", &file]).run()),
    );
    t.section(
        "dibs batch -  (one line from stdin)",
        &n.output(
            &s.dibs(["batch", "-"])
                .stdin("dibs --label from-stdin true\n")
                .run(),
        ),
    );

    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let file = batch_file(
        &s,
        "cancelled",
        &[
            &format!(
                "[hold cont] dibs --label batch-hold '{}; {}'",
                up.signal(),
                hold.hold()
            ),
            "[after] dibs --label batch-after true",
        ],
    );
    let (out, err) = (s.path("cancelled.out"), s.path("cancelled.err"));
    let driver = s.spawn(s.dibs(["batch", &file]).streams_to(&out, &err));
    up.reached();
    let id = capture(&s.read("cancelled.err"), r"^dibs: batch ([0-9-]*), ").unwrap();
    let kill = s.dibs(["--kill", &id]).run();
    s.wait(driver);
    t.section(
        "dibs --kill <batch>  (where its driver runs)",
        &n.output(&kill),
    );
    t.section(
        "what the cancelled batch's driver printed",
        &n.output(&Output {
            code: 76,
            stdout: s.read("cancelled.out"),
            stderr: s.read("cancelled.err"),
        }),
    );
    snapshot("batch", t.text());
}

const LOG_FIXTURE: &str = "\
2026-09-01T10:00:00-04:00\tarrived\t1001\tshared\tbuild\t-\t-\t-\tcargo build\tan agent
2026-09-01T10:00:09-04:00\tfinished\t1001\tshared\tbuild\t0\t9\t0\tcargo build\tan agent
2026-09-02T11:00:00-04:00\tarrived\t1002\tbench\tgemm\t-\t-\t-\tcargo bench\tan agent on gpu:card\t20260902-110000-7 measure
2026-09-02T11:03:00-04:00\tfinished\t1002\tbench\tgemm\t5\t175\t0\tcargo bench\tan agent on gpu:card\t20260902-110000-7 measure
2026-09-03T12:00:00-04:00\tarrived\t1003\tshared\ta-very-long-label-that-is-cut\t-\t-\t-\tan exceedingly long command line that goes on well past fifty characters\tan agent whose name is longer than twenty-two\t-\t20260903120000-1003
2026-09-03T12:00:01-04:00\tbypassed\t1003\tshared\ta-very-long-label-that-is-cut\t-\t-\t-\tan exceedingly long command line that goes on well past fifty characters\tan agent whose name is longer than twenty-two\t-\t20260903120000-1003
2026-09-03T12:00:02-04:00\tkilled\t1004\tkill\t1003\t-\t-\t-\tkilled shared x (pid 1003) after 1s: sleep\tsomeone else\t-\t-
2026-09-03T12:00:02-04:00\taborted\t1003\tshared\ta-very-long-label-that-is-cut\t-\t-\t-\tan exceedingly long command line that goes on well past fifty characters\tan agent whose name is longer than twenty-two\t-\t20260903120000-1003
2026-09-04T13:00:00-04:00\tpeek\t1005\tpeek\tpeek\t-\t0\t0\tnvidia-smi\t\t-\t-
";

#[test]
fn log() {
    let s = Sandbox::new();
    let n = Normal::paths(&s);
    let mut t = Transcript::default();
    t.section(
        "dibs --log  (with nothing logged)",
        &n.output(&s.dibs(["--log"]).run()),
    );
    s.write("log", LOG_FIXTURE);
    t.section("dibs --log 20", &n.output(&s.dibs(["--log", "20"]).run()));
    t.section("dibs --log 2", &n.output(&s.dibs(["--log", "2"]).run()));
    snapshot("log", t.text());
}

#[test]
fn list() {
    let s = Sandbox::new();
    let dir = app(&s);
    recipes(
        &s,
        &format!(
            "{PARAMS}\n[bench.fresh]\nfresh = [\"STORE\"]\n  [[bench.fresh.step]]\n  lock = \"exclusive\"\n  run = \"echo $STORE\"\n\n[service.servers]\nbuild = \"true\"\n\n[[service.servers.serve]]\nname = \"api\"\nrun = \"true\"\n\n[tree]\nfresh = [\"cache\"]\n"
        ),
    );
    s.write(
        "home/.config/dibs/recipes/app.toml",
        "[test.mine]\n  [[test.mine.step]]\n  lock = \"shared\"\n  run = \"true\"\n",
    );
    let n = Normal::of(&s);
    let mut t = Transcript::default();
    t.section("dibs list <app>", &n.output(&s.dibs(["list", &dir]).run()));
    fs::remove_file(s.path("home/.config/dibs/recipes/app.toml")).unwrap();
    t.section(
        "dibs list <a clone with no recipes>",
        &n.output(&s.dibs(["list", &s.p("home/prog/app")]).run()),
    );
    snapshot("list", t.text());
}

const RUNS_FIXTURE: &str = r#"{"t":1767268800,"verb":"bench","label":"app/bench/gemm","repo":"app","fingerprint":"aaa","machine":"box-a","procedure":[{"lock":"shared","run":"cargo build"},{"lock":"exclusive","run":"cargo bench"}],"revisions":{"app":"abc123def456"},"steps":[{"lock":"shared","status":0,"seconds":30},{"lock":"exclusive","status":0,"seconds":120}],"outcome":"ok"}
{"t":1767272400,"verb":"bench","label":"app/bench/gemm","repo":"app","fingerprint":"aaa","machine":"box-a","procedure":[{"lock":"shared","run":"cargo build"},{"lock":"exclusive","run":"cargo bench"}],"revisions":{"app":"abc123def456"},"steps":[{"lock":"shared","status":0,"seconds":2},{"lock":"exclusive","status":0,"seconds":126}],"outcome":"ok"}
{"t":1767276000,"verb":"bench","label":"app/bench/gemm","repo":"app","fingerprint":"bbb","machine":"box-a","procedure":[{"lock":"shared","run":"cargo build --release"},{"lock":"exclusive","run":"cargo bench"}],"revisions":{"app":"abc123def456"},"steps":[{"lock":"shared","status":0,"seconds":40},{"lock":"exclusive","status":0,"seconds":118}],"outcome":"ok","anyway":true}
{"t":1767279600,"verb":"bench","label":"app/bench/sweep@gpu:card","repo":"app","variant":"app-topk","fingerprint":"ccc","machine":"box-b","params":{"backend":"vulkan","samples":"30"},"state":{"governor":"performance"},"reps":3,"procedure":[],"revisions":{"app":"local:abc123-0123456789ab"},"steps":[{"lock":"shared","status":0,"seconds":90},{"lock":"exclusive","status":0,"seconds":12,"rep":1},{"lock":"exclusive","status":0,"seconds":10,"rep":2},{"lock":"exclusive","status":0,"seconds":11,"rep":3}],"outcome":"ok","new_series":true,"seeded":"app-local-0123456789","fresh":{"STORE":"dibs-1-2"}}
{"t":1767283200,"verb":"bench","label":"app/bench/ab","repo":"app","fingerprint":"ddd","machine":"box-a","refs":"main..local","arms":[{"name":"base","revisions":{"app":"626548f5aa"}},{"name":"local","revisions":{"app":"local:8f12+dirty-ab"}}],"reps":2,"procedure":[],"revisions":{},"steps":[{"lock":"shared","status":0,"seconds":90,"arm":"base"},{"lock":"exclusive","status":0,"seconds":40,"arm":"base","rep":1},{"lock":"exclusive","status":0,"seconds":30,"arm":"local","rep":1},{"lock":"exclusive","status":0,"seconds":32,"arm":"local","rep":2},{"lock":"exclusive","status":0,"seconds":42,"arm":"base","rep":2}],"outcome":"ok"}
{"t":1767286800,"verb":"build","label":"app/build/fails","repo":"app","fingerprint":"eee","machine":"box-a","procedure":[{"lock":"shared","run":"exit 3"}],"revisions":{"app":"abc123def456"},"steps":[{"lock":"shared","status":3,"seconds":1}],"outcome":"failed"}
{"t":1767290400,"verb":"shell","label":"app/shell","repo":"app","fingerprint":"","machine":"box-a","reason":"a one-off nan check","procedure":[{"lock":"shared","run":"true"}],"revisions":{"app":"abc123def456"},"steps":[{"lock":"shared","status":0,"seconds":3}],"outcome":"ok"}
{"t":1767294000,"verb":"raw","label":"raw","fingerprint":"","reason":"a one-off nan check","procedure":[{"lock":"shared","run":"nvidia-smi"}],"revisions":{},"steps":[{"lock":"shared","status":0,"seconds":0}],"outcome":"ok"}
{"t":1767297600,"verb":"raw","label":"raw","fingerprint":"","reason":"checking a pull request by hand","procedure":[{"lock":"shared","run":"git log"}],"revisions":{},"steps":[{"lock":"shared","status":0,"seconds":0}],"outcome":"ok"}
not json, a torn line
"#;

const FRICTION_FIXTURE: &str = r#"{"t":1767268800,"text":"--stream does nothing inside a batch","by":"an agent","dibs":"abc1234"}
{"t":1767355200,"text":"--stream does nothing inside a batch.","by":"another agent","dibs":"def5678"}
{"t":1767441600,"text":"the trailer says built=nothing but the recipe did build","by":"","dibs":""}
"#;

#[test]
fn runs_and_gaps() {
    let s = Sandbox::new();
    let n = Normal::of(&s);
    let utc = |args: &[&str]| s.dibs(args).env("TZ", "UTC").run();
    let mut t = Transcript::default();
    t.section(
        "dibs runs  (with nothing recorded)",
        &n.output(&utc(&["runs"])),
    );
    t.section(
        "dibs gaps  (with nothing recorded)",
        &n.output(&utc(&["gaps"])),
    );
    s.write("home/.local/state/dibs/runs.jsonl", RUNS_FIXTURE);
    s.write("home/.local/state/dibs/friction.jsonl", FRICTION_FIXTURE);
    for args in [
        &["runs"][..],
        &["runs", "--all"],
        &["runs", "gemm"],
        &["runs", "app/build/fails"],
        &["runs", "app/build/fails", "--all"],
        &["runs", "nothing-by-this-name"],
        &["gaps"],
    ] {
        t.section(
            &format!("TZ=UTC dibs {}", typed(args)),
            &n.output(&utc(args)),
        );
    }
    snapshot("runs", t.text());
}

pub(crate) const INVENTORY: &str = r#"default = "box-a"
root = "~/prog"

[machine.box-a]
ssh      = "dibs@box-a"
hostname = "box-a"
probed   = "2026-09-01"
workstation = true

  [[machine.box-a.device]]
  kind  = "cpu"
  name  = "a processor"
  cores = 16

  [[machine.box-a.device]]
  kind     = "gpu"
  alias    = "gpu:card"
  name     = "a card"
  pci      = "0000:01:00.0"
  chip     = "10de:2786"
  link     = "x16 of x16 at 16GT/s"
  runtimes = ["cuda", "vulkan"]

  [[machine.box-a.device]]
  kind     = "gpu"
  alias    = "gpu:other"
  name     = "another card"
  pci      = "0000:02:00.0"
  chip     = "10de:2786"
  runtimes = ["cuda", "vulkan"]

[machine.box-b]
ssh      = "box-b.local"
hostname = "box-b"
measure  = false
"#;

#[test]
fn machines() {
    let mut s = Sandbox::new();
    let n = Normal::of(&s);
    let mut t = Transcript::default();
    t.section(
        "dibs --machines  (with no inventory)",
        &n.output(
            &s.dibs(["--machines"])
                .env("DIBS_MACHINES", s.p("none.toml"))
                .run(),
        ),
    );
    s.machines(INVENTORY);
    for args in [&["--machines"][..], &["--machines", "-v"]] {
        t.section(
            &format!("dibs {}", typed(args)),
            &n.output(&s.dibs(args).run()),
        );
    }
    snapshot("machines", t.text());
}

#[test]
fn check() {
    if cfg!(target_os = "macos") {
        skip(
            "on macOS, --check there reports another platform's machine, and its snapshot is not written yet",
        );
        return;
    }
    // What a machine reports of itself is its own, so the tools it reads cards from are stubs,
    // its scratch cannot be written, and only what no stub reaches is normalised.
    let mut s = Sandbox::new();
    s.write_exec(
        "bin/nvidia-smi",
        "#!/bin/sh\necho 'NVIDIA GeForce RTX 4090, 00000000:7F:00.0, 24564, 8.9'\n",
    );
    for tool in ["rocm-smi", "rocminfo"] {
        s.write_exec(&format!("bin/{tool}"), "#!/bin/sh\nexit 1\n");
    }
    s.write_exec("bin/lspci", "#!/bin/sh\necho '7f:00.0 VGA compatible controller [0300]: NVIDIA Corporation AD102 [GeForce RTX 4090] [10de:2684] (rev a1)'\n");
    fs::create_dir_all(s.path("ro-scratch")).unwrap();
    s.command("chmod", ["555", &s.p("ro-scratch")]).run();
    s.set("DIBS_SCRATCH", s.p("ro-scratch"));
    fs::create_dir_all(s.path("home/prog/app/.git")).unwrap();
    s.machines("[machine.box-a]\nssh = \"dibs@box-a\"\nhostname = \"box-a\"\n");
    let n = Normal::of(&s)
        .rule(r"(ok    bash) [0-9][^,]*,", "$1 <version>,")
        .rule(r"(ok    rsync) \S+,", "$1 <version>,")
        .rule(
            r"(?m)^(    cpu   ).*, [0-9]+ threads$",
            "$1<model>, <n> threads",
        )
        .rule(r#"(?m)^(  name  = )".*"$"#, r#"$1"<model>""#)
        .rule(r"(?m)^(  cores = )[0-9]+$", "$1<n>")
        .rule(r#"(probed   = )"[0-9-]+""#, r#"$1"<date>""#)
        .rule(r"(?m)^(measure  = false|workstation = true).*\n", "")
        .rule(
            r"(?m)^  warn  \S+ reaches the host over x.*\n(        .*\n){2}",
            "",
        )
        .rule(
            r"([0-9]+) blocking, [0-9]+ to look at",
            "$1 blocking, <n> to look at",
        )
        .rule(r"usable, [0-9]+ thing", "usable, <n> thing");
    let mut t = Transcript::default();
    t.section("dibs --check", &n.output(&s.dibs(["--check"]).run()));
    t.section(
        "dibs --on box-a --check --write  (on box-a itself)",
        &n.output(&s.dibs(["--on", "box-a", "--check", "--write"]).run()),
    );
    t.section("the inventory it wrote", &n.apply(&s.read("machines.toml")));
    snapshot("check", t.text());
}

/// A scratch to sweep, each entry a different size so the listing's order is the same on any
/// filesystem.
fn scratch_to_sweep(s: &Sandbox) -> String {
    let sized = [
        ("ws/demo/stale", 400_000, "30 days ago"),
        ("ws/demo/fresh", 200_000, "now"),
        ("target/demo", 100_000, "now"),
        ("target/demo-arm1", 300_000, "9 days ago"),
        ("tmp/left", 20_000, "30 days ago"),
        ("jobs/20250901120000-1", 5_000, "30 days ago"),
        ("byhand", 10_000, "now"),
    ];
    for (dir, size, when) in sized {
        s.write(&format!("gc/{dir}/blob"), &"x".repeat(size));
        let dated = match dir.starts_with("ws/") || dir.starts_with("target/") {
            true => {
                s.write(&format!("gc/{dir}/.dibs-used"), "");
                format!("gc/{dir}/.dibs-used")
            }
            false => format!("gc/{dir}"),
        };
        assert_eq!(s.command("touch", ["-d", when, &s.p(&dated)]).code(), 0);
    }
    s.p("gc")
}

#[test]
fn gc() {
    let s = Sandbox::new();
    let g = scratch_to_sweep(&s);
    let n = Normal::of(&s)
        .clocked()
        .rule(r"\b[0-9]+(\.[0-9])?[KMG]\b", "<size>")
        .rule(
            r"(?m)^  \S+ free of \S+ on \S+$",
            "  <free> free of <size> on <mount>",
        )
        .rule(r"(build caches on) \S+,", "$1 <mount>,");
    let gc = |args: &[&str]| s.dibs(args).env("DIBS_SCRATCH", &g).run();
    let mut t = Transcript::default();
    for args in [
        &["--gc", "--dry-run"][..],
        &["--gc", "--days", "2", "--dry-run"],
        &["--gc"],
    ] {
        t.section(&format!("dibs {}", typed(args)), &n.output(&gc(args)));
    }
    t.section(
        "dibs --gc  (with nothing under scratch)",
        &n.output(
            &s.dibs(["--gc"])
                .env("DIBS_SCRATCH", s.p("no-scratch"))
                .run(),
        ),
    );
    snapshot("gc", t.text());
}

#[test]
fn out_and_fetch() {
    let mut s = Sandbox::new();
    let n = Normal::of(&s)
        .clocked()
        .pids()
        .rule(r"\(([0-9]+[hms][0-9hms]*)\)", "(<dur>)")
        .rule(r"\([0-9]+ bytes", "(<n> bytes")
        .rule(r"ran [0-9]+s  exit", "ran <s>  exit");
    let mut t = Transcript::default();
    let finished = job_id(&s.dibs(["--label", "done", "seq 1 50"]).run().stderr);
    let (up, release) = (s.gate("up"), s.gate("release"));
    let cmd = format!(
        "bash -c '{{ echo first line; echo second line; {}; {}; }} > {} 2>&1'",
        up.signal(),
        release.hold(),
        s.p("job.log")
    );
    let running = s.spawn(s.dibs(["--label", "writes-a-log", &cmd]));
    up.reached();
    let pid = s.records("holder")[0][1].clone();
    let calls: Vec<(&str, Vec<&str>)> = vec![
        ("dibs --out", vec!["--out"]),
        ("dibs --out <pid>", vec!["--out", &pid]),
        (
            "dibs out <finished job>  (the first read, which keeps it here)",
            vec!["out", &finished],
        ),
        (
            "dibs out <finished job>  (read again, from what was kept)",
            vec!["out", &finished],
        ),
        ("dibs --out 999999", vec!["--out", "999999"]),
        ("dibs --out 19700101-1", vec!["--out", "19700101-1"]),
        (
            "dibs --fetch <finished job>  (which kept no files)",
            vec!["--fetch", &finished],
        ),
        ("dibs --fetch 19700101-1", vec!["--fetch", "19700101-1"]),
    ];
    for (title, args) in &calls {
        t.section(title, &n.output(&s.dibs(args).run()));
    }
    release.open();
    s.wait(running);
    t.section(
        "dibs --out  (with nothing running)",
        &n.output(&s.dibs(["--out"]).run()),
    );
    snapshot("out", t.text());
}

#[test]
fn kill_and_release() {
    let mut s = Sandbox::new();
    let n = status_clock(Normal::of(&s))
        .rule(r"(Sent SIG[A-Z]+ to)( <?[a-z0-9]+>?)+ \(", "$1 <pids> (");
    let mut t = Transcript::default();
    let (p, q) = (s.gate("p"), s.gate("q"));
    let blocker = s.spawn(
        s.dibs(["--bench", "--label", "blocker", &p.hold()])
            .session("owner"),
    );
    s.held(1);
    let pid = s.pid_of("blocker").to_string();
    let calls: Vec<(&str, Call)> = vec![
        (
            "dibs --kill <pid>  (another session's)",
            s.dibs(["--kill", &pid]).session("stranger"),
        ),
        (
            "dibs --kill <pid>  (from a shell with no session)",
            s.dibs(["--kill", &pid]).no_session(),
        ),
        ("dibs --kill 999999", s.dibs(["--kill", "999999"])),
        (
            "dibs --kill <pid> --anyone",
            s.dibs(["--kill", &pid, "--anyone"]).session("stranger"),
        ),
    ];
    for (title, call) in calls {
        t.section(title, &n.output(&call.run()));
    }
    s.wait(blocker);
    let own = s.spawn(s.dibs(["--label", "mine", &q.hold()]).session("owner"));
    s.held(1);
    let pid = s.pid_of("mine").to_string();
    t.section(
        "dibs --kill <pid> --force  (its own)",
        &n.output(&s.dibs(["--kill", &pid, "--force"]).session("owner").run()),
    );
    s.wait(own);
    t.section(
        "dibs --release  (with nothing held)",
        &n.output(&s.dibs(["--release"]).run()),
    );
    snapshot("kill", t.text());
}

/// A call to box-a over an ssh that fails as a real one does, beside a tailscale that says what
/// it is told to.
fn unreachable_with(s: &Sandbox, says: &str, tailscale: &str, accepts: &str) -> Output {
    s.write_exec(
        "fails/ssh",
        &format!(
            "#!/bin/bash\nfor a; do [ \"$a\" = -v ] && [ -n \"{accepts}\" ] && echo 'debug1: Server accepts key: /keys/id_ed25519 ED25519 SHA256:x explicit' >&2; done\nprintf '%s\\n' '{says}' >&2\nexit 255\n"
        ),
    );
    s.write_exec(
        "fails/tailscale",
        &format!("#!/bin/sh\nprintf '%s\\n' '{tailscale}'\n"),
    );
    s.dibs(["--on", "box-a", "--status"])
        .env("PATH", format!("{}:{}", s.p("fails"), s.var("PATH")))
        .env("DIBS_LOCAL", "0")
        .env("DIBS_CONNECT_TIMEOUT", "2")
        .run()
}

#[test]
fn unreachable() {
    let mut s = Sandbox::new();
    s.machines(INVENTORY);
    let n = Normal::of(&s);
    let mut t = Transcript::default();
    let timed_out = "ssh: connect to host box-a port 22: Connection timed out";
    let cases = [
        (
            "a host key that changed",
            "@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@",
            "",
            "",
        ),
        (
            "a host key never recorded",
            "Host key verification failed.",
            "",
            "",
        ),
        (
            "a refused login, with no key it takes",
            "dibs@box-a: Permission denied (publickey).",
            "",
            "",
        ),
        (
            "a refused login, with a key it takes locked",
            "dibs@box-a: Permission denied (publickey).",
            "",
            "1",
        ),
        (
            "a refused connection",
            "ssh: connect to host box-a port 22: Connection refused",
            "",
            "",
        ),
        (
            "a name that does not resolve",
            "ssh: Could not resolve hostname box-a: Name or service not known",
            "",
            "",
        ),
        (
            "no route from here",
            "ssh: connect to host box-a port 22: Network is unreachable",
            "",
            "",
        ),
        (
            "nothing at its address",
            "ssh: connect to host box-a port 22: No route to host",
            "",
            "",
        ),
        (
            "a timeout, with tailscale logged out",
            timed_out,
            "Logged out.",
            "",
        ),
        (
            "a timeout, with tailscale stopped",
            timed_out,
            "Tailscale is stopped.",
            "",
        ),
        (
            "a timeout, with the peer offline",
            timed_out,
            "100.64.0.2   box-a   someone@   linux   offline",
            "",
        ),
        (
            "a timeout, with the peer up",
            timed_out,
            "100.64.0.2   box-a   someone@   linux   -",
            "",
        ),
        (
            "a timeout, and not a peer",
            timed_out,
            "100.64.0.3   box-c   someone@   linux   -",
            "",
        ),
    ];
    for (what, says, tailscale, accepts) in cases {
        t.section(
            &format!("dibs --on box-a --status  ({what})"),
            &n.output(&unreachable_with(&s, says, tailscale, accepts)),
        );
    }
    let from_host = s
        .dibs(["--status"])
        .env("PATH", format!("{}:{}", s.p("fails"), s.var("PATH")))
        .env("DIBS_LOCAL", "0")
        .env("DIBS_HOST", "dibs@box-z")
        .env("DIBS_MACHINES", s.p("none.toml"))
        .run();
    t.section(
        "DIBS_HOST=dibs@box-z dibs --status  (with no inventory, and box-z timing out)",
        &n.output(&from_host),
    );
    s.write_exec("fails/ssh", "#!/bin/sh\nexit 70\n");
    let full = s
        .dibs(["--on", "box-a", "true"])
        .env("PATH", format!("{}:{}", s.p("fails"), s.var("PATH")))
        .env("DIBS_LOCAL", "0")
        .run();
    t.section(
        "dibs --on box-a true  (a machine whose scratch cannot be written)",
        &n.output(&full),
    );
    snapshot("unreachable", t.text());
}

#[test]
fn notices() {
    let mut s = Sandbox::new();
    s.machines(&format!(
        "{INVENTORY}\n[machine.here]\nssh      = \"here\"\nhostname = \"{}\"\n",
        hostname()
    ));
    let n = Normal::of(&s)
        .clocked()
        .pids()
        .rule(r"(?m)^rsync[: ].*\n", "");
    let mut t = Transcript::default();
    let away = |args: &[&str]| {
        s.dibs(args)
            .env("DIBS_LOCAL", "0")
            .env("DIBS_CONNECT_TIMEOUT", "2")
            .run()
    };
    t.section(
        "dibs --on box-a --bench true  (two GPUs, and none named)",
        &n.output(&away(&["--on", "box-a", "--bench", "true"])),
    );
    s.write("series", "#dibs-series 1\nmoved\there\tgpu:card\tan agent\t1\t3\nstarted-elsewhere\tdibs@box-a\tnone\tan agent\t1\t5\n");
    for args in [
        &["--on", "here", "--bench", "--label", "moved", "true"][..],
        &[
            "--on",
            "here",
            "--bench",
            "--label",
            "started-elsewhere",
            "true",
        ],
        &[
            "--on",
            "here",
            "--bench",
            "--new-series",
            "--label",
            "moved",
            "exit 1",
        ],
    ] {
        t.section(
            &format!("dibs {}", typed(args)),
            &n.output(&s.dibs(args).run()),
        );
    }
    for args in [
        &["--on", "box-a", "--sync", "-a", "./x", ":~/y"][..],
        &["--on", "box-a", "--sync", "-a", ":~/y", "./x"],
    ] {
        t.section(&format!("dibs {}", typed(args)), &n.output(&away(args)));
    }
    let inner = format!("{DIBS} --label inner true");
    t.section(
        "dibs --hold --label nest '<a dibs call that takes the lock>'",
        &n.output(&s.dibs(["--hold", "--label", "nest", &inner]).run()),
    );
    snapshot("notices", t.text());
}

/// A call that is refused, and what it takes to be refused that way.
pub(crate) struct Refusal {
    args: Vec<String>,
    env: Vec<(String, String)>,
    stdin: Option<String>,
}

impl Refusal {
    pub(crate) fn of(args: &[&str]) -> Refusal {
        Refusal {
            args: args.iter().map(|a| a.to_string()).collect(),
            env: Vec::new(),
            stdin: None,
        }
    }

    pub(crate) fn env(mut self, key: &str, value: &str) -> Refusal {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    fn stdin(mut self, text: &str) -> Refusal {
        self.stdin = Some(text.to_string());
        self
    }
}

/// Each refusal's exit in one table, then what each one said.
pub(crate) fn refusal_table(s: &Sandbox, n: &Normal, refusals: Vec<Refusal>) -> String {
    let mut table = String::from("exit  call\n");
    let mut t = Transcript::default();
    for r in refusals {
        let args: Vec<&str> = r.args.iter().map(String::as_str).collect();
        let mut call = s.dibs(&args);
        let mut title = format!("dibs {}", typed(&args)).trim_end().to_string();
        for (k, v) in r.env.iter().rev() {
            call = call.env(k, v);
            title = format!("{k}={} {title}", typed(&[v.as_str()]));
        }
        if let Some(text) = &r.stdin {
            call = call.stdin(text);
            title = format!(
                "{title} <<< {}",
                typed(&[text.trim_end()]).replace('\n', "\\n")
            );
        }
        let title = n.apply(&title);
        let out = call.run();
        table.push_str(&format!("{:<4}  {title}\n", out.code));
        t.section(&title, &n.output(&out));
    }
    format!("{table}\n{}", t.text())
}

#[test]
fn refusals() {
    let s = Sandbox::new();
    s.write("two.toml", "[machine.box-a]\nssh = \"dibs@box-a\"\nhostname = \"box-a\"\n\n[machine.box-b]\nssh = \"dibs@box-b\"\nhostname = \"box-b\"\nmeasure = false\n");
    s.write("cards.toml", INVENTORY);
    fs::create_dir_all(s.path("ro-lock")).unwrap();
    s.command("chmod", ["555", &s.p("ro-lock")]).run();
    let (two, cards, ro) = (s.p("two.toml"), s.p("cards.toml"), s.p("ro-lock"));
    let none = "/nonexistent/machines.toml";
    let help = s.dibs(["--help"]).run().stdout;
    let n = Normal::of(&s)
        .literal(&help, "<what dibs --help prints>\n")
        .rule(r"\.writable\.[0-9]+", ".writable.<pid>")
        .rule(r"(bash: line) [0-9]+:", "$1 <n>:");
    let r = Refusal::of;
    let refusals = vec![
        r(&["--bogus"]),
        r(&["--kill"]),
        r(&["--forget"]),
        r(&["--prefer"]),
        r(&["--repo"]),
        r(&["--gc", "--days"]),
        r(&["--wait"]),
        r(&["--max"]),
        r(&["--label"]),
        r(&["--device"]),
        r(&["--with", "s=true", "--ready"]),
        r(&["--with", "./serve", "true"]),
        r(&["--with", "a-b=true", "true"]),
        r(&["--with", "s=true", "--with", "s=false", "true"]),
        r(&["--ready", "tcp:1", "true"]),
        r(&["--port", "8080", "true"]),
        r(&["--port", "api", "--port", "api", "true"]),
        r(&["--ready-within", "soon", "true"]),
        r(&["--with", "s=true", "--ready", "tcp:nope", "true"]),
        r(&["--peek", "--with", "s=true", "true"]),
        r(&["--peek", "--hold", "true"]),
        r(&["--hold", "--device", "gpu:x", "true"]),
        r(&["--label", "x", "list", "app"]),
        r(&[]),
        r(&["--status", "extra"]),
        r(&["--release", "extra"]),
        r(&["--log", "5", "extra"]),
        r(&["--check", "a", "b"]),
        r(&["--fetch"]),
        r(&["--fetch", "notanid"]),
        r(&["--fetch", "1-2", "dir", "extra"]),
        r(&["--out", "5", "extra"]),
        r(&["--kill", "notapid"]),
        r(&["--watch", "1"]),
        r(&["--watch", "5", "extra"]),
        r(&["--gc", "extra"]),
        r(&["--gc", "--days", "x"]),
        r(&["--dry-run", "true"]),
        r(&["--days", "3", "true"]),
        r(&["--sync", ":~/x"]),
        r(&["--sync", "./a", "./b"]),
        r(&["--sync", "--on", "x", "./a", ":~/b"]),
        r(&["--sync", "./x", ":$DIBS_SCRATCH/x"]),
        r(&["--label", "x", "true"]).env("DIBS_LOCK_DIR", &ro),
        r(&["--label", "x", "true"]).env("DIBS_HOLDING", hostname()),
        r(&["--friction", "   "]),
        r(&["--machines"]).env("DIBS_MACHINES", none),
        r(&["--on", "nope", "--status"]).env("DIBS_MACHINES", none),
        r(&["--on", "nope", "--status"]).env("DIBS_MACHINES", &two),
        r(&["--forget", "nope"]).env("DIBS_MACHINES", &two),
        r(&["--on", "box-b", "--bench", "true"]).env("DIBS_MACHINES", &two),
        r(&["--bench", "true"])
            .env("DIBS_MACHINES", &two)
            .env("DIBS_LOCAL", "0"),
        r(&["--bench", "true"])
            .env("DIBS_MACHINES", &two)
            .env("DIBS_LOCAL", "0")
            .env("DIBS_HOST", "dibs@box-z"),
        r(&["--peek", "true"])
            .env("DIBS_MACHINES", &two)
            .env("DIBS_LOCAL", "0"),
        r(&["true"])
            .env("DIBS_MACHINES", none)
            .env("DIBS_LOCAL", "0"),
        r(&["--which"])
            .env("DIBS_MACHINES", &two)
            .env("DIBS_LOCAL", "0"),
        r(&["--which"])
            .env("DIBS_MACHINES", none)
            .env("DIBS_LOCAL", "0")
            .env("DIBS_HOST", "dibs@box-z"),
        r(&["--which"]).env("DIBS_MACHINES", none),
        r(&["--check", "--write"])
            .env("DIBS_MACHINES", none)
            .env("DIBS_LOCAL", "0"),
        r(&["--on", "box-a", "--device", "gpu:nope", "true"]).env("DIBS_MACHINES", &cards),
        r(&["--on", "box-a", "--device", "card", "true"]).env("DIBS_MACHINES", &cards),
        r(&["--on", "box-a", "--device", "gpu:card", "true"]).env("DIBS_MACHINES", &two),
        r(&["--device", "gpu:card", "true"]).env("DIBS_MACHINES", none),
        r(&["--pick", "--repo", "absent"])
            .env("DIBS_MACHINES", &two)
            .env("DIBS_LOCAL", "0")
            .env("DIBS_POLL_TIMEOUT", "3"),
    ];
    snapshot("refusals", &refusal_table(&s, &n, refusals));
}

#[test]
fn recipe_refusals() {
    let s = Sandbox::new();
    let dir = app(&s);
    let cargo = fake_cargo(&s);
    recipes(
        &s,
        &format!(
            "{PARAMS}\n[bench.stolen]\n  [[bench.stolen.step]]\n  lock = \"shared\"\n  run = \"{cargo} build\"\n  [[bench.stolen.step]]\n  lock = \"shared\"\n  run = \"echo /another/tree > \\\"$CARGO_TARGET_DIR/.dibs-tree\\\"\"\n  [[bench.stolen.step]]\n  lock = \"exclusive\"\n  run = \"echo measured\"\n\n[service.servers]\nbuild = \"true\"\n\n[[service.servers.serve]]\nname = \"api\"\nrun = \"true\"\n"
        ),
    );
    s.write("two.toml", "[machine.box-a]\nssh = \"dibs@box-a\"\nhostname = \"box-a\"\n\n[machine.box-b]\nssh = \"dibs@box-b\"\nhostname = \"box-b\"\n");
    fs::create_dir_all(s.path("loose")).unwrap();
    let n = Normal::of(&s)
        .clocked()
        .pids()
        .rule(r"\b[0-9a-f]{12}\b", "<sha>");
    let (local, main) = (format!("{dir}@local"), format!("{dir}@main"));
    let two = s.p("two.toml");
    let r = Refusal::of;
    let refusals = vec![
        r(&["build"]),
        r(&["build", &local]),
        r(&["build", &local, "nope"]),
        r(&["build", &local, "p", "--backend", "metal"]),
        r(&["build", &local, "p", "--backends", "cuda"]),
        r(&["build", &local, "need"]),
        r(&["build", &local, "rel"]),
        r(&["bench", &local, "hot"]),
        r(&["build", &local, "p", "--reps", "0"]),
        r(&["build", &local, "p", "--sweep", "samples"]),
        r(&["build", &local, "p", "--sweep", "backend=cuda,metal"]),
        r(&["build", &local, "p", "--device", "nope"]),
        r(&["build", &local, "p", "--reason"]),
        r(&["build", &local, "p", "-x"]),
        r(&["build", &local, "p", "--"]),
        r(&["build", &local, "p", "--there"]),
        r(&["bench", &format!("{dir}@main...local"), "hot"]),
        r(&["bench", &format!("{dir}@local,local"), "hot"]),
        r(&["bench", &format!("{dir}@no-such-ref"), "stolen"]),
        r(&["bench", &main, "stolen"]),
        r(&["shell", &local, "--", "true"]),
        r(&[
            "shell", &local, "--reason", "r", "--label", "mine", "--", "true",
        ]),
        r(&[
            "shell",
            &local,
            "--reason",
            "r",
            "--bench",
            "--",
            "cargo bench --bench gemm",
        ]),
        r(&["shell", &s.p("loose"), "--reason", "r", "--", "true"]),
        r(&["raw", "--", "true"]),
        r(&["raw", "--reason", "r"]),
        r(&["with", &local, "servers"]),
        r(&["with", &local, "nope", "--", "true"]),
        r(&[
            "with",
            &format!("{dir}@main..local"),
            "servers",
            "--",
            "true",
        ]),
        r(&["bench", &local, "stolen"])
            .env("DIBS_MACHINES", &two)
            .env("DIBS_LOCAL", "0"),
        r(&["batch", &s.p("no-such-batch")]),
        r(&["batch", "-"]).stdin("cargo build\n"),
        r(&["batch", "-"]).stdin("[a] dibs true\n[a] dibs true\n"),
        r(&["batch", "-"]).stdin("[a after=b] dibs true\n"),
        r(&["batch", "-"]).stdin("[a after=b] dibs true\n[b after=a] dibs true\n"),
        r(&["batch", "-"]).stdin("[a bogus=1] dibs true\n"),
        r(&["batch", "-"]).stdin("[a dibs true\n"),
        r(&["batch", "-"]).stdin("dibs true; dibs false\n"),
        r(&["batch", "-"]).stdin("dibs true \\\n"),
        r(&["batch", "-"]).stdin("# only a comment\n"),
        r(&["batch", "-"])
            .stdin("[b] dibs --bench --label nameless true\n")
            .env("DIBS_MACHINES", &two)
            .env("DIBS_LOCAL", "0"),
    ];
    snapshot("refusals-recipes", &refusal_table(&s, &n, refusals));
}
