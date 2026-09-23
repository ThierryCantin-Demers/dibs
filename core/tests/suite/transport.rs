use crate::harness::*;
use std::fs;
use std::io::Write;
use std::time::Duration;

fn signal_group(pid: u32, sig: i32) {
    unsafe { libc::kill(-(pid as i32), sig) };
}

fn base64(text: &str) -> String {
    let out = std::process::Command::new("base64").arg("-w0").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    out.stdin.as_ref().unwrap().write_all(text.as_bytes()).unwrap();
    String::from_utf8(out.wait_with_output().unwrap().stdout).unwrap()
}

#[test]
fn a_command_near_the_argument_limit_reaches_the_machine() {
    // The script and the command used to travel as one ssh argument, capped at 128KB, which the
    // script alone nearly filled.
    let s = Sandbox::new();
    let big = format!("b='{}'; echo ${{#b}}", "x".repeat(100_000));
    assert_eq!(s.remote(s.dibs(["--label", "transport-big", &big])).run().stdout, "100000\n", "a command near the argument limit reaches the machine");
    assert_eq!(s.dibs(["--label", "transport-big-local", &big]).run().stdout, "100000\n", "and one run here");
}

#[test]
fn the_jobs_exit_comes_back_and_nothing_is_left_on_the_machine() {
    let s = Sandbox::new();
    assert_eq!(s.remote(s.dibs(["--label", "transport-exit", "exit 7"])).code(), 7, "the job's exit comes back");
    assert_eq!(fs::read_dir(s.path("remote-run")).unwrap().count(), 0, "no script is left on the machine");
}

#[test]
fn a_dead_callers_job_is_noticed_through_the_stream() {
    // Without the parent-death signal, the caller's death has to reach the far side as EOF on the
    // stream the script and command arrived on.
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let call = s.remote(s.dibs(["--label", "transport-hangup", &format!("{}; {}", up.signal(), never.hold())])).env("DIBS_NO_PDEATHSIG", "1");
    let caller = s.spawn(call);
    up.reached();
    unsafe { libc::kill(caller.pid as i32, libc::SIGKILL) };
    s.wait(caller);
    s.log_line("caller-gone.*transport-hangup");
    s.gone();
}

#[test]
fn a_caller_that_stops_answering_is_let_go_when_its_lease_runs_out() {
    // A laptop that sleeps closes nothing. Stopping the caller's group is that, as long as the far
    // side runs in a session of its own and so keeps going, as a machine does.
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let call = s.command("setsid", [DIBS, "--label", "lease-asleep", &format!("{}; {}", up.signal(), never.hold())]);
    let caller = s.spawn(s.remote(call).env("DIBS_LEASE", "2"));
    up.reached();
    signal_group(caller.pid, libc::SIGSTOP);
    s.log_line("caller-gone.*lease-asleep.*caller silent for 2s");
    s.gone();
    signal_group(caller.pid, libc::SIGCONT);
    s.wait(caller);
}

#[test]
fn a_caller_that_answers_outlasts_many_leases() {
    let s = Sandbox::new();
    let never = s.gate("never");
    let cmd = format!("read -r -t 4 _ <> {}; echo held", never.path.display());
    let out = s.remote(s.dibs(["--label", "lease-alive", &cmd])).env("DIBS_LEASE", "1").run();
    assert_eq!(out.stdout, "held\n");
}

#[test]
fn a_watch_whose_caller_stops_answering_stops_redrawing() {
    // A watch costs the machine a render on every tick, for nobody.
    let mut s = Sandbox::new();
    let call = s.command("setsid", [DIBS, "--watch", "2"]);
    let caller = s.spawn(s.remote(call).env("DIBS_LEASE", "2"));
    let far = format!("^bash {}/.dibs-payload", s.p("remote-run"));
    let mut pid = 0;
    until("the watch to reach the machine", || {
        let out = s.command("pgrep", ["-f", &far]).run().stdout;
        pid = out.lines().next().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
        pid != 0
    });
    signal_group(caller.pid, libc::SIGSTOP);
    // Its parent is the stopped caller, so the far side stays a zombie once it has ended.
    let state = || fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|st| st.rsplit(')').next().and_then(|r| r.split_whitespace().next().map(str::to_string)));
    until("the far side to end", || matches!(state().as_deref(), None | Some("Z")));
    assert_eq!(state().as_deref(), Some("Z"));
    signal_group(caller.pid, libc::SIGCONT);
    signal_group(caller.pid, libc::SIGTERM);
    s.wait(caller);
}

#[test]
fn a_heartbeat_is_not_a_tick() {
    // Every heartbeat woke the watch, so it redrew far more often than its interval: renders the
    // machine pays for, and ticks a reader cannot tell the interval from.
    let s = Sandbox::new();
    let out = s.remote(s.dibs(["--watch", "3", "--json"])).env("DIBS_LEASE", "1").within(Duration::from_secs(4)).run().stdout;
    let ticks = out.lines_with("\"state\"");
    assert!(ticks <= 2, "{ticks} redraws in 4s of a 3s watch");
}

#[test]
fn a_sync_crosses_the_transport_intact_both_ways() {
    // rsync's own protocol follows the script and command on the same stream.
    let s = Sandbox::new();
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let blob: Vec<u8> = (0..2_000_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    fs::create_dir_all(s.path("tsrc/sub")).unwrap();
    fs::write(s.path("tsrc/sub/blob"), &blob).unwrap();
    s.remote(s.dibs(["--sync", "-a", "--no-times", "--checksum", &format!("{}/", s.p("tsrc")), &format!(":{}/", s.p("tdst"))])).run();
    assert!(fs::read(s.path("tdst/sub/blob")).ok() == Some(blob.clone()), "a sync sends a file intact");
    s.remote(s.dibs(["--sync", "-a", &format!(":{}/", s.p("tdst")), &format!("{}/", s.p("tback"))])).run();
    assert!(fs::read(s.path("tback/sub/blob")).ok() == Some(blob), "and fetches it back intact");
}

#[test]
fn a_caller_that_goes_away_takes_the_whole_job_with_it() {
    // The channel closing is how the machine learns its caller is gone, and the lock is released as
    // soon as the job exits, so anything still running below the job would run unlocked.
    let mut s = Sandbox::new();
    let payload = s.payload();
    let (ready, block) = (s.gate("ready"), s.gate("block"));
    s.write("grand.sh", &format!("echo $$ > {}\n{}\n{}\n", s.p("grand.pid"), ready.signal(), block.hold()));
    s.write("mid.sh", &format!("sh {}\necho mid-done\n", s.p("grand.sh")));
    let cmd = base64(&format!("bash {}; echo after", s.p("mid.sh")));
    let args = ["shared", "gone-caller", "0", "0", "0", &cmd, "0", "0", "", "0", "", "", "", "", "", "1", "0"];
    let mut argv = vec![payload.as_str()];
    argv.extend(args);
    let (job, channel) = s.spawn_fed(s.command("bash", argv));
    ready.reached();
    let grandchild: u32 = s.read("grand.pid").trim().parse().unwrap();
    assert!(alive(grandchild), "the job reached its grandchild");
    drop(channel);
    s.wait(job);
    assert!(!alive(grandchild), "the grandchild is gone once the job has ended");
    assert_eq!(s.holders(), 0, "and the lock with it");
}

#[test]
fn a_transfers_far_half_carries_its_stream_untouched() {
    // rsync's transport never reaches the machine from here, so the far half is run as rsync would.
    let s = Sandbox::new();
    let payload = s.payload();
    let cmd = base64("echo carried");
    let out = s.command("bash", [payload.as_str(), "rsh", "sync", "0", "0", "0", &cmd, "1", "0", "", "0", "", "", "", "", "", "1", "0", "5"]).run();
    assert_eq!(out.code, 0, "a transfer's far half exits with its command");
    assert_eq!(out.stdout, "carried\n", "and carries its stream untouched");
    assert_eq!(out.stderr.lines_with("unbound variable"), 0, "with nothing unbound on the way out");
}
