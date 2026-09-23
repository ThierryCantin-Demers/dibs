use crate::harness::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[test]
fn a_benchmark_excludes_everyone_until_it_ends() {
    let mut s = Sandbox::new();
    assert_eq!(s.status().lines_with("dibs: idle"), 1, "idle to start");
    let a = s.gate("a");
    let bench = s.spawn(s.dibs(["--bench", "--label", "bench-A", &a.hold()]));
    s.held(1);
    assert_eq!(s.status().lines_with("BUSY, benchmark"), 1, "a benchmark reads as busy");
    assert_eq!(s.dibs(["--wait", "1", "--label", "build-B", "echo no"]).code(), 75, "it excludes shared users");
    assert_eq!(s.dibs(["--bench", "--wait", "1", "--label", "bench-C", "echo no"]).code(), 75, "it excludes other benchmarks");
    a.open();
    s.wait(bench);
    assert_eq!(s.holders(), 0, "and frees on normal exit");
}

#[test]
fn shared_users_run_together() {
    let mut s = Sandbox::new();
    let (g1, g2) = (s.gate("s1"), s.gate("s2"));
    let b1 = s.spawn(s.dibs(["--label", "build-1", &g1.hold()]));
    s.held(1);
    let b2 = s.spawn(s.dibs(["--label", "build-2", &g2.hold()]));
    s.held(2);
    assert_eq!(s.holders(), 2);
    g1.open();
    g2.open();
    s.wait(b1);
    s.wait(b2);
}

#[test]
fn a_queued_benchmark_gates_later_shared_users() {
    let mut s = Sandbox::new();
    let long = s.gate("long");
    let holder = s.spawn(s.dibs(["--label", "build-long", &long.hold()]));
    s.held(1);
    let bench = s.spawn(s.dibs(["--bench", "--label", "bench-q", "echo ran"]).stdout_to(&s.path("q")));
    s.queued(1);
    assert_eq!(s.status().lines_with("queued 1 of 1: bench"), 1, "a benchmark queues with a position");
    assert_eq!(s.dibs(["--wait", "1", "--label", "build-late", "echo late"]).code(), 75, "and gates later shared users");
    long.open();
    s.wait(holder);
    s.wait(bench);
    assert_eq!(s.read("q").lines_with("ran"), 1, "then runs");
}

#[test]
fn a_queued_caller_is_told_at_once_what_it_is_behind() {
    let mut s = Sandbox::new();
    let g = s.gate("qn");
    let holder = s.spawn(s.dibs(["--bench", "--label", "qn-holder", &g.hold()]));
    s.held(1);
    let out = s.dibs(["--wait", "1", "--label", "queued-notice", "echo nope"]).run().all();
    assert_eq!(out.lines_with("queued and has not started"), 1, "a queued caller is told at once, not after the wait");
    assert_eq!(
        out.lines_matching(r"^dibs: queued and has not started, behind the benchmark qn-holder[,.].* dibs status shows the queue\.$"),
        1,
        "in one line saying what it is behind"
    );
    let verbose = s.dibs(["-v", "--wait", "1", "--label", "queued-notice", "echo nope"]).run().all();
    let told = |text: &str| between(text, "queued and has not started", "gave up").lines_with("BUSY");
    assert_eq!((told(&out), told(&verbose)), (0, 1), "without the whole queue, which is what -v is for");
    g.open();
    s.wait(holder);
}

#[test]
fn a_killed_benchmark_frees_the_lock() {
    let mut s = Sandbox::new();
    let d = s.gate("d");
    let doomed = s.spawn(s.dibs(["--bench", "--label", "doomed", &d.hold()]));
    s.held(1);
    let _ = std::process::Command::new("pkill").args(["-9", "-P", &doomed.pid.to_string()]).status();
    unsafe { libc::kill(doomed.pid as i32, libc::SIGKILL) };
    s.wait(doomed);
    // The record outlives a SIGKILL because no trap can run; the lock does not, because it is an
    // open descriptor. So what is asked is whether the next job can get in.
    let out = s.dibs(["--bench", "--wait", "5", "--label", "after-kill", "echo recovered"]).run();
    assert_eq!(out.stdout.lines_with("recovered"), 1, "the next benchmark gets the lock");
    assert_eq!(s.status().lines_with("dibs: idle"), 1, "and status prunes the dead record");
}

#[test]
fn max_kills_an_overrun_and_records_no_duration() {
    let s = Sandbox::new();
    let m = s.gate("m");
    assert_eq!(s.dibs(["--bench", "--max", "2", "--label", "runaway", &m.hold()]).code(), 124, "--max kills an overrun");
    assert_eq!(s.read("history").lines_with("runaway"), 0, "an overrun is not recorded as a duration");
}

#[test]
fn a_quick_job_goes_around_a_queued_benchmark() {
    let mut s = Sandbox::new();
    s.history("shared\tquickie\t1\nshared\tquickie\t1\nshared\tquickie\t1\n");
    s.history("shared\tslowpoke\t120\nshared\tslowpoke\t120\nshared\tslowpoke\t120\n");
    let (anchor, blocked, quick, slow, unseen) = (s.gate("an"), s.gate("bl"), s.gate("by"), s.gate("sl"), s.gate("nh"));
    let a = s.spawn(s.dibs(["--label", "anchor", &anchor.hold()]));
    s.held(1);
    let b = s.spawn(s.dibs(["--bench", "--label", "blocked", &blocked.hold()]));
    s.queued(1);

    fn patient(s: &Sandbox, label: &str, gate: &Gate, patience: &str) -> Call {
        s.dibs(["--label", label, &gate.hold()]).env("DIBS_PATIENCE", patience).env("DIBS_QUICK", "5")
    }
    let q1 = s.spawn(patient(&s, "quickie", &quick, "600"));
    s.held(2);
    assert_eq!(s.holders(), 2, "a quick job goes around it");
    assert_eq!(s.status().lines_with("queued 1 of 1: bench"), 1, "and the bench is still queued, not passed over");
    assert_eq!(s.dibs(["--log", "20"]).run().stdout.lines_with("bypassed"), 1, "the log says it went around");
    quick.open();
    s.wait(q1);

    let q2 = s.spawn(patient(&s, "quickie", &quick, "0"));
    s.queued(2);
    assert_eq!(s.waiters(), 2, "past the bench's patience it waits its turn");
    let sl = s.spawn(patient(&s, "slowpoke", &slow, "600"));
    s.queued(3);
    assert_eq!(s.waiters(), 3, "a job too slow to qualify waits");
    let nh = s.spawn(patient(&s, "never-seen", &unseen, "600"));
    s.queued(4);
    assert_eq!(s.waiters(), 4, "and so does one with no history of its own");

    anchor.open();
    s.wait(a);
    blocked.open();
    s.wait(b);
    for g in [&quick, &slow, &unseen] {
        g.open();
    }
    for j in [q2, sl, nh] {
        s.wait(j);
    }
    s.gone();
}

#[test]
fn peek_ignores_the_lock_and_registers_nothing() {
    let mut s = Sandbox::new();
    let p = s.gate("p");
    let blocker = s.spawn(s.dibs(["--bench", "--label", "blocker", &p.hold()]));
    s.held(1);
    assert_eq!(s.dibs(["--peek", "echo peeked"]).run().stdout, "peeked\n", "--peek ignores the lock");
    assert_eq!(s.status().lines_with("peek"), 0, "and does not register");
    p.open();
    s.wait(blocker);
}

#[test]
fn kill_stops_only_what_you_mean_to() {
    let mut s = Sandbox::new();
    let p = s.gate("p");
    let blocker = s.spawn(s.dibs(["--bench", "--label", "blocker", &p.hold()]));
    s.held(1);
    let pid = s.pid_of("blocker").to_string();
    // Several agents read the same --status, so a pid copied out of it belongs to whoever happens
    // to be holding the machine. Stopping someone else's measurement has to be deliberate.
    let out = s.dibs(["--kill", &pid]).session("killer").run();
    assert_eq!(out.code, 2, "--kill refuses a job that is not yours");
    assert_eq!(out.all().lines_with("belongs to"), 1, "and names who it belongs to");
    assert_eq!(s.holders(), 1, "and the job is still running");
    let out = s.dibs(["--kill", &pid, "--anyone"]).session("killer").run();
    assert_eq!(out.all().lines_with("It belonged to"), 1, "--anyone is how you mean it");
    s.gone();
    s.wait(blocker);
    assert_eq!(s.log().lines_matching("killed.*blocker"), 1, "a kill is logged with its target");
    assert!(s.log().lines_with("aborted") >= 1, "and the job it tore down is logged too");
    assert_eq!(s.dibs(["--kill", "999999"]).code(), 1, "--kill refuses an unknown pid");
}

#[test]
fn a_job_is_owned_by_its_session_not_its_title() {
    // A title goes stale, two sessions can share one, and one that changed mid-run would make an
    // agent a stranger to its own job.
    let mut s = Sandbox::new();
    let q = s.gate("q");
    let job = s.spawn(s.dibs(["--label", "ident", &q.hold()]).session("ident"));
    s.held(1);
    let rec = &s.records("holder")[0];
    assert_eq!(rec[5], "local_ident", "the record carries the session");
    assert_eq!(rec[4], "session ident", "and the title beside it");
    assert!(!rec[6].is_empty(), "the command still lands in the last field");
    q.open();
    s.wait(job);
}

#[test]
fn a_job_an_account_started_is_not_anyones_to_stop_by_default() {
    // A session with no id is named after the account, and every shell of that account shares it.
    let mut s = Sandbox::new();
    let n = s.gate("n");
    let job = s.spawn(s.dibs(["--label", "acct", &n.hold()]).no_session());
    s.held(1);
    let owner = &s.records("holder")[0][5];
    assert!(regex::Regex::new("^shell-[^@]+@").unwrap().is_match(owner), "the record names the account and the laptop: {owner}");
    let pid = s.pid_of("acct").to_string();
    let out = s.dibs(["--kill", &pid]).no_session().run();
    assert_eq!(out.code, 2, "the same account cannot stop it without saying so");
    assert_eq!(out.all().lines_with("names an account"), 1, "and it says why");
    assert_eq!(s.dibs(["--kill", &pid, "--anyone"]).no_session().code(), 0, "--anyone stops it");
    s.wait(job);
    s.gone();
}

#[test]
fn a_session_that_named_its_work_is_told_apart() {
    let mut s = Sandbox::new();
    let n = s.gate("n");
    let job = s.spawn(s.dibs(["--label", "named", &n.hold()]).no_session().env("DIBS_AGENT", "sweep a"));
    s.held(1);
    let pid = s.pid_of("named").to_string();
    let kill_as = |who: &str| s.dibs(["--kill", &pid]).no_session().env("DIBS_AGENT", who).code();
    assert_eq!(kill_as("sweep b"), 2, "a session that named its work is told apart from another");
    assert_eq!(kill_as("sweep a"), 0, "and can stop its own");
    s.wait(job);
    s.gone();
}

#[test]
fn every_user_on_a_machine_takes_the_same_lock() {
    // State under /run/user or /tmp is keyed by uid, so two people each took their own lock, each
    // was told the machine was idle, and both benchmarked at once.
    let mut s = Sandbox::new();
    s.unset("DIBS_LOCK_DIR");
    let shared = s.p("shared-lock");
    let check = s.dibs(["--check"]).env("DIBS_SHARED_LOCK_DIR", &shared).run().stdout;
    assert_eq!(check.lines_with("keyed to this uid"), 1, "with no shared directory the lock is keyed to a uid, and --check says so");
    // Told only to make a group, someone with one account for everybody fixes a problem they do
    // not have.
    assert_eq!(check.lines_with("everyone this one account"), 1, "and names both remedies, not only the group");
    assert_eq!(
        check.lines_with(&format!("install -d -m 2775 -g dibs {shared}")),
        1,
        "and tells you exactly how to fix it, naming the configured path"
    );
    fs::create_dir_all(&shared).unwrap();
    let check = s.dibs(["--check"]).env("DIBS_SHARED_LOCK_DIR", &shared).run().stdout;
    assert_eq!(check.lines_with(&format!("lock directory is shared: {shared}")), 1, "with one present it is used");
    s.dibs(["--label", "shared-dir", "echo ran"]).env("DIBS_SHARED_LOCK_DIR", &shared).run();
    assert!(s.exists("shared-lock/rw"), "and a job actually takes its lock there");
    // A record has to be removable by whoever prunes it, or one user's dead job wedges the queue
    // for everyone else.
    let mut records: Vec<_> = fs::read_dir(&shared).unwrap().flatten().map(|e| e.path()).collect();
    records.sort();
    let mode = fs::metadata(&records[0]).unwrap().permissions().mode();
    assert_ne!(mode & 0o020, 0, "records are group-writable so another user can prune them");
}

#[test]
fn a_lock_directory_it_cannot_write_is_refused_rather_than_run_around() {
    // mkdir -p succeeds on a directory that is there and cannot be written, which is what a
    // sandboxed shell sees, and flock then failed and the command ran with no lock at all.
    let s = Sandbox::new();
    let ro = s.path("ro-lock");
    fs::create_dir_all(&ro).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    let out = s.dibs(["--label", "rocheck", "echo ran-anyway"]).env("DIBS_LOCK_DIR", ro.display().to_string()).run();
    assert_eq!(out.code, 71, "it exits 71");
    assert_eq!(out.all().lines_with("ran-anyway"), 0, "and the command did not run");
    assert_eq!(out.all().lines_with("Nothing was run"), 1, "and it says nothing ran");
}

/// Holds the lock the way the remains of a session that has gone would: from a session of its
/// own, since held from here it would share a process group with every `--status` asked.
fn orphan(s: &mut Sandbox, mode: &str, up: &Gate, release: &Gate) -> Job {
    let script = format!("exec 8>\"$1/rw\"; flock {mode} 8; printf 'up\\n' > \"$2\"; read -r _ < \"$3\"");
    let lockdir = s.var("DIBS_LOCK_DIR");
    let call = s.command("setsid", ["bash", "-c", &script, "_", &lockdir, &up.path.display().to_string(), &release.path.display().to_string()]);
    s.spawn(call)
}

#[test]
fn an_orphaned_lock_names_what_holds_it() {
    let mut s = Sandbox::new();
    let (up, release) = (s.gate("up"), s.gate("release"));
    let o = orphan(&mut s, "-s", &up, &release);
    up.reached();
    let out = s.dibs(["--status"]).run().all();
    release.open();
    assert_eq!(out.lines_with("LOCKED BY AN ORPHAN"), 1, "it is reported as an orphan");
    assert_eq!(out.lines_matching("holding it:|reports holding it"), 1, "and it says what is holding it");
    // It named its own shipped script alongside the orphan, so the advice underneath was partly to
    // kill the dibs that was answering.
    let named = between(&out, "holding it:", "Stop it with");
    let named: Vec<_> = named.lines().skip(1).take_while(|l| !l.contains("Stop it with")).filter(|l| !l.is_empty()).collect();
    assert_eq!(named.len(), 1, "and names only it, not the dibs answering: {named:?}");
    s.wait(o);
    assert_eq!(s.status().lines_with("dibs: idle"), 1, "back to idle once released");
}

#[test]
fn an_orphaned_lock_is_reclaimed_not_only_named() {
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let _o = orphan(&mut s, "-x", &up, &never);
    up.reached();
    // Nothing releases a lock whose holder has no session left to end, so a caller behind it is
    // waiting on a wedge, not a queue.
    let out = s.dibs(["--wait", "1", "--label", "behind-orphan", "echo no"]).run();
    assert_eq!(out.all().lines_with("held by an orphan"), 1, "a caller behind one is told what it is waiting for");
    assert_eq!(out.code, 75, "and gives up rather than queueing into it");
    assert_eq!(s.dibs(["--release"]).run().all().lines_with("Reclaiming it"), 1, "--release reclaims it");
    assert_eq!(s.status().lines_with("dibs: idle"), 1, "the machine is usable again");
    assert_eq!(s.log().lines_with("reclaimed"), 1, "and what was reclaimed is in the log");
}

#[test]
fn a_live_holder_is_not_an_orphan() {
    // A holder whose record was misread is a running command, and ending it would spoil the
    // measurement it is in the middle of.
    let mut s = Sandbox::new();
    let g = s.gate("rl");
    let job = s.spawn(s.dibs(["--label", "release-safe", &g.hold()]));
    s.held(1);
    assert_eq!(s.dibs(["--release"]).run().all().lines_with("Reclaiming"), 0, "--release leaves a recorded holder alone");
    assert_eq!(s.holders(), 1, "and it is still holding");
    g.open();
    s.wait(job);
    s.gone();
}

#[test]
fn a_client_asking_about_the_lock_it_is_queueing_for_is_not_an_orphan_of_itself() {
    // A queued client prints the status while holding the lock descriptor, so every child that
    // asks inherits it and fuser reports them all. Thirteen clients arriving at once were each
    // read as an orphan of the other twelve.
    let s = Sandbox::new();
    let script = "exec 8>\"$1/rw\"; flock -s 8
        printf \"shared\\t%s\\t%s\\tinq\\tsomeone\\tid\\t-\\ttrue\\n\" \"$$\" \"$(date +%s)\" > \"$1/waiting.$$\"
        \"$2\" --status > \"$3\" 2>&1; \"$2\" --status --json > \"$4\" 2>&1
        rm -f \"$1/waiting.$$\"";
    let (lockdir, out, json) = (s.var("DIBS_LOCK_DIR"), s.p("inq.out"), s.p("inq.json"));
    s.command("bash", ["-c", script, "_", &lockdir, DIBS, &out, &json]).run();
    let text = s.read("inq.out");
    assert_eq!(text.lines_with("ORPHAN"), 0, "--status does not call it an orphan");
    assert_eq!(text.lines_with("just taken the lock"), 1, "it says the lock has just been taken");
    // Routing reads --json, and two renderers asking one question in two places is how they come
    // to disagree.
    assert_eq!(capture(&s.read("inq.json"), r#""state":"([^"]*)""#).as_deref(), Some("busy"), "and --json agrees with it");
    assert_eq!(s.status().lines_with("dibs: idle"), 1, "still idle afterwards");
}

#[test]
fn a_pid_that_comes_round_again_is_not_the_job_that_had_it() {
    // pid_max comes round every few days on a busy machine, so nothing may rest on a pid naming one
    // process forever, nor on two jobs of one day never sharing one.
    let mut s = Sandbox::new();
    let out = s.dibs(["--label", "pidwrap-id", "true"]).run();
    assert!(
        regex::Regex::new(r"^[0-9]{14}-[0-9]+$").unwrap().is_match(&job_id(&out.stderr)),
        "a job id carries the time, not only the day and the pid"
    );
    let g = s.gate("pw");
    let other = s.spawn(s.sh(&g.hold()));
    // Written ten minutes ago by a job that has since ended, and the pid now belongs to a process
    // that started well after it, which is what a wraparound leaves behind.
    let ghost = |s: &Sandbox| {
        let then = (now() - 600).to_string();
        let pid = other.pid.to_string();
        s.record("holder", other.pid, &["shared", &pid, &then, "pidwrap-ghost", "an agent", "a-session", "-", "a job that ended without clearing up"]);
        s.command("touch", ["-d", &format!("@{then}"), &s.lockdir().join(format!("holder.{pid}")).display().to_string()]).run();
    };
    ghost(&s);
    assert_eq!(s.status().lines_with("pidwrap-ghost"), 0, "a record older than the process now holding its pid is not that job");
    assert!(!s.lockdir().join(format!("holder.{}", other.pid)).exists(), "and it is cleared");
    ghost(&s);
    let out = s.dibs(["--kill", &other.pid.to_string()]).run();
    assert_eq!(out.code, 1, "--kill refuses it rather than signalling whatever has that pid");
    assert_eq!(out.all().lines_with("ended without clearing its record"), 1, "saying why");
    assert!(alive(other.pid), "and the process it would have killed is untouched");
    g.open();
    s.wait(other);
}

#[test]
fn a_flag_whose_value_is_missing_is_refused_not_read_for_ever() {
    // shift 2 with one argument left shifts nothing, so the loop read the same flag again: a typo
    // became a hang the caller could not interrupt, on --kill of all things.
    let s = Sandbox::new();
    for flag in ["--kill", "--job", "--cancel", "--forget", "--prefer", "--repo"] {
        assert_eq!(s.dibs([flag]).within(Duration::from_secs(5)).code(), 2, "{flag} alone exits rather than hanging");
    }
    assert_eq!(s.dibs(["--gc", "--days"]).within(Duration::from_secs(5)).code(), 2, "--days alone exits rather than hanging");
}

#[test]
fn a_signal_to_the_machine_script_alone_takes_its_job_with_it() {
    // --kill signals the job's whole tree, but anything else that signals the script itself, a
    // session going away or a plain kill, released the lock and left the job running unlocked.
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let cmd = format!("echo $$ > {}; {}; {}", s.p("job.pid"), up.signal(), never.hold());
    let caller = s.spawn(s.dibs(["--label", "term-main", &cmd]));
    up.reached();
    let main: i32 = s.records("holder")[0][1].parse().unwrap();
    let job: u32 = s.read("job.pid").trim().parse().unwrap();
    unsafe { libc::kill(main, libc::SIGTERM) };
    until("the job to stop", || !alive(job));
    s.wait(caller);
    s.gone();
    assert_eq!(s.dibs(["--bench", "--wait", "5", "--label", "after-term", "true"]).code(), 0, "and the lock is free");
}

#[test]
fn a_signal_to_a_queued_machine_script_takes_it_out_of_the_queue_at_once() {
    // Not when it would finally have been let in: nobody is waiting for it any more.
    let mut s = Sandbox::new();
    let hold = s.gate("hold");
    let bench = s.spawn(s.dibs(["--bench", "--label", "blocker", &hold.hold()]));
    s.held(1);
    let queued = s.spawn(s.dibs(["--label", "queued", "true"]));
    s.queued(1);
    let main: i32 = s.records("waiting")[0][1].parse().unwrap();
    unsafe { libc::kill(main, libc::SIGTERM) };
    s.until_records("the queued job to leave", || s.waiters() == 0);
    s.wait(queued);
    assert_eq!(s.holders(), 1, "and the benchmark is untouched");
    hold.open();
    s.wait(bench);
}

/// Ctrl+C at a terminal: SIGINT to every process in the call's foreground group.
fn ctrl_c(remote: bool) {
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let cmd = format!("echo $$ > {}; {}; {}", s.p("job.pid"), up.signal(), never.hold());
    let call = s.dibs(["--label", "ctrl-c", &cmd]).own_group();
    let caller = s.spawn(if remote { s.remote(call) } else { call });
    up.reached();
    let job: u32 = s.read("job.pid").trim().parse().unwrap();
    unsafe { libc::kill(-(caller.pid as i32), libc::SIGINT) };
    until("the job to stop", || !alive(job));
    s.wait(caller);
    s.gone();
}

#[test]
fn ctrl_c_on_a_call_here_stops_its_job_and_frees_the_lock() {
    // A job started in the background of a script ignores SIGINT, so the one thing Ctrl+C reaches
    // on the job's behalf is the script, which has to stop the job before it lets the lock go.
    ctrl_c(false);
}

#[test]
fn ctrl_c_on_a_call_to_another_machine_stops_its_job_there() {
    ctrl_c(true);
}

#[test]
fn a_caller_killed_outright_here_takes_its_job_with_it() {
    // Over ssh the machine sees its channel close. Here there is no channel, and a SIGKILL takes
    // nothing down with it, so the job held the lock until it ended or ran into --max.
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let cmd = format!("echo $$ > {}; {}; {}", s.p("job.pid"), up.signal(), never.hold());
    let caller = s.spawn(s.dibs(["--label", "killed-here", &cmd]));
    up.reached();
    let job: u32 = s.read("job.pid").trim().parse().unwrap();
    unsafe { libc::kill(caller.pid as i32, libc::SIGKILL) };
    s.wait(caller);
    until("the job to stop", || !alive(job));
    s.gone();
}

#[test]
fn a_caller_killed_outright_here_while_queued_leaves_the_queue() {
    let mut s = Sandbox::new();
    let hold = s.gate("hold");
    let bench = s.spawn(s.dibs(["--bench", "--label", "blocker", &hold.hold()]));
    s.held(1);
    let caller = s.spawn(s.dibs(["--label", "queued", "true"]));
    s.queued(1);
    unsafe { libc::kill(caller.pid as i32, libc::SIGKILL) };
    s.wait(caller);
    until("the queued job to leave", || s.waiters() == 0);
    hold.open();
    s.wait(bench);
}
