use crate::harness::*;
use std::time::Duration;

fn holder_record(s: &Sandbox, mode: &str, label: &str, agent: &str, what: &str, age: u64) {
    let (pid, then) = (live_pid(), (now() - age).to_string());
    s.record("holder", pid, &[mode, &pid.to_string(), &then, label, agent, what]);
}

#[test]
fn an_estimate_reaches_for_the_agent_before_the_machine() {
    // Most labels on the real machine appear once, because agents name the run rather than the
    // kind of work. What that agent's other jobs took is the next best answer.
    let s = Sandbox::new();
    s.history("bench\tsome-run\t300\tAgent One\nbench\tanother-run\t300\tAgent One\n");
    s.history("bench\tunrelated\t5\tAgent Two\nbench\tunrelated\t5\tAgent Two\n");
    holder_record(&s, "bench", "novel-run", "Agent One", "the new one", 0);
    assert_eq!(
        s.status().lines_with("this agent's other bench jobs: usually 5m00s over 2"),
        1,
        "it reaches for the agent before the machine"
    );
    assert_eq!(s.status_json()["holders"][0]["est_scope"], "agent", "and the json names that scope");
    s.history("bench\tnovel-run\t60\tAgent One\nbench\tnovel-run\t60\tAgent One\n");
    assert_eq!(s.status().lines_with("usually 1m00s over 2 runs"), 1, "its own history wins when it has one");
    holder_record(&s, "bench", "never-run", "Agent Three", "the new one", 0);
    assert_eq!(
        s.status().lines_with("every bench job on the machine:"),
        1,
        "a label and an agent both unseen fall through to the mode"
    );
}

#[test]
fn a_holder_is_estimated_from_its_label_and_the_queue_gets_an_eta() {
    let mut s = Sandbox::new();
    s.history("bench\teta\t600\nbench\teta\t660\nbench\teta\t620\n");
    let e = s.gate("e");
    let holder = s.spawn(s.dibs(["--bench", "--label", "eta", &e.hold()]));
    s.held(1);
    let waiter = s.spawn(s.dibs(["--bench", "--label", "eta", "--wait", "30", "echo q1"]));
    s.queued(1);
    let status = s.status();
    assert_eq!(status.lines_with("usually 10m20s over 3 runs"), 1, "median of 600,620,660 is 10m20s");
    assert_eq!(status.lines_with("until it starts"), 1, "the queue gets an ETA");
    e.open();
    s.wait(holder);
    s.wait(waiter);
}

#[test]
fn an_unfamiliar_label_says_the_number_is_not_its_own() {
    let mut s = Sandbox::new();
    s.history("bench\teta\t600\nbench\teta\t660\nbench\teta\t620\n");
    let n = s.gate("n");
    let holder = s.spawn(s.dibs(["--bench", "--label", "unseen", &n.hold()]));
    s.held(1);
    assert_eq!(s.status().lines_with("nothing on this one"), 1);
    n.open();
    s.wait(holder);
}

#[test]
fn with_no_history_it_invents_no_duration() {
    let mut s = Sandbox::new();
    let z = s.gate("z");
    let holder = s.spawn(s.dibs(["--bench", "--label", "blank", &z.hold()]));
    s.held(1);
    let waiter = s.spawn(s.dibs(["--wait", "30", "--label", "waiter", "echo x"]));
    s.queued(1);
    let status = s.status();
    assert_eq!(status.lines_with("no history for this one yet"), 1, "with no history it invents no duration");
    assert_eq!(status.lines_with("until it starts"), 0, "and no ETA for the queue");
    assert_eq!(status.lines_with("queued 1 of 1"), 1, "but the waiter still has a position");
    z.open();
    s.wait(holder);
    s.wait(waiter);
}

#[test]
fn a_job_that_never_starts_working_is_flagged() {
    // -1 flags anything the rate is willing to call idle, whatever its age, and an hour flags
    // nothing: the age guard and the CPU reading are separate claims, checked as such.
    let mut s = Sandbox::new();
    let w = s.gate("w");
    let job = s.spawn(s.dibs(["--bench", "--label", "wedged", &w.hold()]));
    s.held(1);
    let look = |after: &str| s.dibs(["--status"]).env("DIBS_IDLE_AFTER", after).run().stdout;
    // The idle signal is a rate, so the first look only leaves a reading behind to compare with.
    look("-1");
    // Which of the two idle sentences it gets depends on whether the fixture burned a whole tick,
    // which is not under test here; that it is flagged at all is.
    assert_eq!(look("-1").lines_with("IDLE:"), 1, "a job that never starts working is flagged");
    assert_eq!(look("-1").lines_with("dibs --kill"), 1, "and it is told how to stop it");
    assert_eq!(look("3600").lines_with("IDLE"), 0, "one younger than the threshold is left alone");
    w.open();
    s.wait(job);
}

#[test]
fn a_job_that_worked_is_judged_by_what_its_reaped_children_did() {
    let mut s = Sandbox::new();
    let (burned, release, behind) = (s.gate("burned"), s.gate("release"), s.gate("behind"));
    // CPU time rather than wall time: on a loaded machine a second of clock buys a fraction of
    // that in CPU, and the assertion would measure the load instead of the counting.
    let burner = format!(
        "for i in 1 2 3; do python3 -c 'import time\ns = time.process_time()\nwhile time.process_time() - s < 1.2: pass'; done\n{}\n{}",
        burned.signal(),
        release.hold()
    );
    let busy = s.spawn(s.dibs(["--bench", "--label", "busy", &burner]));
    s.held(1);
    let queued = s.spawn(s.dibs(["--label", "behind", &behind.hold()]));
    s.queued(1);
    burned.reached();
    let look = |no_children: &str| {
        s.dibs(["--status"]).env("DIBS_IDLE_AFTER", "-1").env("DIBS_NO_CHILDREN", no_children).run().stdout
    };
    // The burner has stopped by here, so what separates these two looks is only that the second
    // has something to compare against. A cumulative count could not tell them apart at all.
    assert_eq!(look("0").lines_with("IDLE"), 0, "a job that has worked is not flagged on first sight");
    assert_eq!(look("0").lines_with("none of it in the last"), 1, "one that worked and then stopped is flagged on the next look");
    // A supervisor owns almost no CPU itself: its work was done by children it has reaped, and
    // counting only the living is what made a busy sweep read as idle.
    let cpu = |no_children: &str| -> u64 {
        capture(&look(no_children), r"IDLE: ([0-9]+)s of CPU").and_then(|v| v.parse().ok()).unwrap_or(0)
    };
    assert!(cpu("0") >= 3, "the work its reaped children did is counted");
    // The fallback walk is unreachable on an ordinary kernel and is only ever exercised here.
    assert_eq!(cpu("1"), cpu("0"), "and the fallback walk counts the same");
    release.open();
    s.wait(busy);
    // The queued job waited as long as the burner ran, and what it shows as a holder is what it
    // has been running, not what it has been alive.
    until("the queued job to hold", || s.status().lines_matching("^  shared  behind") == 1);
    assert_eq!(s.status().lines_matching("behind +[0-2]s"), 1, "a holder's clock starts when it acquires, not when it arrived");
    behind.open();
    s.wait(queued);
}

#[test]
fn queued_shared_jobs_do_not_wait_out_each_other() {
    // The shared lock admits them all at once, so the queue only advances at a benchmark. Adding
    // their durations up told the third job it was waiting out the first two.
    let mut s = Sandbox::new();
    s.history("bench\tblocker\t100\nbench\tblocker\t100\nbench\tblocker\t100\n");
    s.history("shared\tbuild-a\t60\nshared\tbuild-a\t60\nshared\tbuild-a\t60\n");
    s.history("shared\tbuild-b\t50\nshared\tbuild-b\t50\nshared\tbuild-b\t50\n");
    s.history("bench\tsweep\t200\nbench\tsweep\t200\nbench\tsweep\t200\n");
    // Real pids, because prune drops any record whose process is gone.
    let q = s.gate("q");
    let waiters: Vec<Job> = (0..3).map(|_| s.spawn(s.sh(&q.hold()))).collect();
    let t = now();
    holder_record(&s, "bench", "blocker", "an agent", "the holder", 0);
    for (job, (mode, label, dt)) in waiters.iter().zip([("shared", "build-a", 0), ("shared", "build-b", 1), ("bench", "sweep", 2)]) {
        let pid = job.pid.to_string();
        s.record("waiting", job.pid, &[mode, &pid, &(t + dt).to_string(), label, "an agent", "queued"]);
    }
    // Relations between the ETAs rather than formatted durations: a second ticking over between
    // writing the records and reading them would otherwise fail one run in several.
    let eta: Vec<i64> = s.status_json()["queue"].as_array().unwrap().iter().map(|e| e["eta"].as_i64().unwrap()).collect();
    assert_eq!(eta.len(), 3, "the queue is the three of them");
    assert_eq!(eta[0], eta[1], "the second shared job does not wait out the first");
    assert_eq!(eta[2] - eta[0], 60, "a benchmark behind them waits for the longest, not the total");
    assert_eq!(s.status().lines_with("until it starts"), 3, "the status display agrees with the json");
    q.open();
    for j in waiters {
        s.wait(j);
    }
}

#[test]
fn a_label_whose_runs_disagree_says_so() {
    // Half the labels on the real machine name a repo rather than a kind of work, so one name
    // covers a git status and a full build. A median is honest there and predicts nothing.
    let s = Sandbox::new();
    s.set_history("shared\tmixed\t0\nshared\tmixed\t0\nshared\tmixed\t0\nshared\tmixed\t200\nshared\tmixed\t240\n");
    holder_record(&s, "shared", "mixed", "an agent", "the mixed job", 30);
    let status = s.status();
    assert_eq!(status.lines_with("anywhere from"), 1, "a label whose runs disagree says so");
    assert_eq!(status.lines_with("usually"), 0, "and does not dress it up as one number");
    let json = s.status_json();
    assert_eq!(json["holders"][0]["est_wide"], true, "the json marks it too");
    // Past the median it is in the tail, and the tail still has a shape. Giving up there cost
    // every waiter its ETA, because the labels that hold a machine longest median at zero.
    assert_eq!(status.lines_with("if it runs true to form"), 1, "past the median it still bounds the wait");
    assert_eq!(json["holders"][0]["remaining_kind"], "bound", "and the json says which kind of answer that is");
}

#[test]
fn a_single_run_does_not_claim_a_habit() {
    let s = Sandbox::new();
    s.set_history("bench\tsolo\t120\n");
    holder_record(&s, "bench", "solo", "an agent", "the only run", 10);
    let status = s.status();
    assert_eq!(status.lines_with("ran once, in 2m00s"), 1, "a single run does not claim a habit");
    assert_eq!(status.lines_with("1 runs"), 0, "and does not say 1 runs");
}

#[test]
fn an_overrun_is_measured_against_the_same_job_not_its_mode() {
    let s = Sandbox::new();
    s.set_history("bench\tother-work\t20\nbench\tother-work\t20\nbench\tother-work\t20\n");
    holder_record(&s, "bench", "long-one", "some agent", "the long job", 600);
    let status = s.status();
    assert_eq!(status.lines_with("STUCK"), 0, "a mode-wide median does not accuse it");
    assert_eq!(status.lines_with("nothing on this one"), 1, "and it says the history is not about this job");
    assert!(!s.dibs(["--status", "--json"]).run().stdout.contains("overrun"), "nor does the json claim an overrun");
    s.history("bench\tlong-one\t20\nbench\tlong-one\t20\nbench\tlong-one\t20\n");
    assert_eq!(s.status().lines_with("STUCK"), 1, "its own median does");
    assert_eq!(s.status_json()["holders"][0]["overrun"], true, "and the json agrees");
}

#[test]
fn a_median_of_zero_accuses_nobody() {
    // Durations are whole seconds, so anything under one records as zero, and "3x its usual 0s"
    // was every run of a quick job.
    let s = Sandbox::new();
    s.set_history("bench\tquick\t0\nbench\tquick\t0\nbench\tquick\t0\n");
    holder_record(&s, "bench", "quick", "some agent", "the quick job", 600);
    let status = s.status();
    assert_eq!(status.lines_with("STUCK"), 0, "a median of zero accuses nobody");
    assert!(!s.dibs(["--status", "--json"]).run().stdout.contains("overrun"), "and the json agrees with the display");
    assert_eq!(status.lines_with("under a second"), 1, "it says what zero seconds means");
}

#[test]
fn an_overdue_job_promises_nothing_behind_it() {
    // Past its usual duration, how much longer it has is not knowable, and zero reads to everyone
    // behind it as "any moment now".
    let s = Sandbox::new();
    s.set_history("bench\tsteady\t100\nbench\tsteady\t100\nbench\tsteady\t100\n");
    holder_record(&s, "bench", "steady", "some agent", "the steady job", 150);
    let parent = std::os::unix::process::parent_id();
    s.record("waiting", parent, &["shared", &parent.to_string(), &now().to_string(), "behind-it", "some agent", "the waiting job"]);
    let status = s.status();
    let json = s.dibs(["--status", "--json"]).run().stdout;
    assert_eq!(status.lines_with("left]"), 0, "an overdue job does not claim a remainder");
    assert_eq!(status.lines_with("longer than it has ever taken"), 1, "it says it is past its usual instead");
    assert!(!json.contains("remaining"), "nor does the json invent one");
    assert_eq!(status.lines_with("until it starts"), 0, "and nothing behind it is promised a start");
    assert!(!json.contains("\"eta\""), "which the json leaves out too");
}

#[test]
fn the_status_says_where_a_jobs_output_is_going() {
    // --out reads a job by finding the file it redirected into, which is useless if nobody knows
    // the feature exists. Naming the file in the status is how it gets discovered.
    let mut s = Sandbox::new();
    let (up, release) = (s.gate("up"), s.gate("release"));
    let log = s.p("redirected.log");
    let cmd = format!("bash -c '{{ echo working; {}; {}; }} > {log} 2>&1'", up.signal(), release.hold());
    let job = s.spawn(s.dibs(["--label", "writes-output", &cmd]));
    up.reached();
    // Matched on the line the status adds, not on the path, which the command line carries too.
    let status = s.status();
    assert_eq!(status.lines_matching("writing .*redirected.log"), 1, "the status names the file");
    assert_eq!(status.lines_with(&format!("dibs --on {} --out", hostname())), 1, "and how to read it, on which machine");
    assert!(s.dibs(["--status", "--json"]).run().stdout.contains("\"output\":"), "the json carries it too");
    release.open();
    s.wait(job);
}

#[test]
fn a_job_writing_to_no_file_says_nothing_about_output() {
    // A line saying there is nothing to read, on every tick of a watch, is noise.
    let mut s = Sandbox::new();
    let (up, release) = (s.gate("up"), s.gate("release"));
    let job = s.spawn(s.dibs(["--label", "no-output", &format!("{}; {}", up.signal(), release.hold())]));
    up.reached();
    assert_eq!(s.status().lines_with("--out"), 0);
    release.open();
    s.wait(job);
}

#[test]
fn the_json_has_a_pinned_shape() {
    // A reader must never have to parse the human display.
    let mut s = Sandbox::new();
    let (j, j2) = (s.gate("j"), s.gate("j2"));
    let holder = s.spawn(s.dibs(["--bench", "--label", "j-holder", &j.hold()]).session("jsonner"));
    s.held(1);
    let d = s.status_json();
    assert_eq!(d["state"], "bench", "the state is named");
    assert_eq!(d["holders"][0]["label"], "j-holder", "the holder is there");
    assert_eq!(d["holders"][0]["agent"], "session jsonner", "with its agent");
    assert!(!s.dibs(["--status", "--json"]).run().stdout.contains('\x1b'), "and no colour ever");
    // Quotes and a backslash are what break hand-rolled JSON.
    let queued = s.spawn(s.dibs(["--label", "j-queued", &format!("echo \"a\\b\" > /dev/null; {}", j2.hold())]));
    s.queued(1);
    let d = s.status_json();
    assert_eq!(d["queue"][0]["label"], "j-queued", "a queued entry survives quoting");
    assert!(d["queue"][0]["cmd"].as_str().unwrap().contains('\\'), "and its command round-trips");
    j.open();
    s.wait(holder);
    j2.open();
    s.wait(queued);
}

#[test]
fn a_job_that_writes_is_a_job_that_works() {
    // A compilation cache runs the compiler in its own daemon, parented to init, so the work
    // happens outside the job's tree and the tree looks idle. Calling that stalled would have
    // dibs tell people to kill healthy builds.
    let mut s = Sandbox::new();
    let w = s.gate("w");
    // Redirected in a child, the way a recipe redirects every step: a holder's own fd 1 is the
    // channel it was launched down and is excluded on purpose.
    let job = s.spawn(s.dibs(["--label", "writes-out", &format!("sh -c '{}' > {}", w.hold(), s.p("written"))]));
    s.held(1);
    until("the job's log to exist", || s.exists("written"));
    let idle = |wrote_within: Option<&str>| {
        let mut call = s.dibs(["--status", "--json"]).env("DIBS_IDLE_AFTER", "-1");
        if let Some(v) = wrote_within {
            call = call.env("DIBS_WROTE_WITHIN", v);
        }
        call.run().stdout.contains("\"idle_for\"")
    };
    assert!(!idle(None), "a stalled tree with a fresh log is not called idle");
    assert!(idle(Some("-1")), "an old log does not rescue it, or the rule could never say idle");
    w.open();
    s.wait(job);
}

#[test]
fn watch_redraws_on_its_interval_and_takes_no_lock() {
    let s = Sandbox::new();
    assert_eq!(s.dibs(["--watch", "1"]).code(), 2, "an interval under the floor is refused");
    assert_eq!(s.dibs(["--watch", "5", "echo hi"]).code(), 2, "and it takes no command");
    let out = s.dibs(["--watch", "2"]).within(Duration::from_millis(2500)).run().stdout;
    assert_eq!(out.lines_with("ctrl-c to stop"), 2, "it redraws on the interval");
    assert!(!out.contains('\x1b'), "piped, it leaves the escape codes out");
    assert_eq!(s.holders(), 0, "and it takes no lock");
}
