use crate::harness::*;
use dibs_format::Mode;
use std::fs;
use std::time::Duration;

fn signal_group(pid: u32, sig: i32) {
    unsafe { libc::kill(-(pid as i32), sig) };
}

#[test]
fn a_command_near_the_argument_limit_reaches_the_machine() {
    // An ssh command line holds at most 128KB and the script alone nearly fills it, so a command
    // travels inside the script, never beside it as an argument.
    let s = Sandbox::new();
    let big = format!("b='{}'; echo ${{#b}}", "x".repeat(100_000));
    assert_eq!(
        s.remote(s.dibs(["--label", "transport-big", &big]))
            .run()
            .stdout,
        "100000\n",
        "a command near the argument limit reaches the machine"
    );
    assert_eq!(
        s.dibs(["--label", "transport-big-local", &big])
            .run()
            .stdout,
        "100000\n",
        "and one run here"
    );
}

#[test]
fn a_command_arrives_as_written_whatever_it_quotes() {
    let s = Sandbox::new();
    let cmd =
        "printf '%s|' \"it's\" '$HOME' 'a\\b' \"tab\there\" \"$((1 + 1))\"\necho 'second line'";
    let written = s.command("bash", ["-c", cmd]).run().stdout;
    assert_eq!(
        s.dibs(["--label", "quoting", cmd]).run().stdout,
        written,
        "here"
    );
    assert_eq!(
        s.remote(s.dibs(["--label", "quoting", cmd])).run().stdout,
        written,
        "and over the transport"
    );
    s.remote(s.dibs(["--label", "quoting", "true"]))
        .env("DIBS_AGENT", "it's \"mine\"")
        .run();
    assert_eq!(
        s.dibs(["--log", "2"])
            .run()
            .stdout
            .lines_with("it's \"mine\""),
        2,
        "and so does who asked"
    );
}

#[test]
fn the_jobs_exit_comes_back_and_nothing_is_left_on_the_machine() {
    let s = Sandbox::new();
    assert_eq!(
        s.remote(s.dibs(["--label", "transport-exit", "exit 7"]))
            .code(),
        7,
        "the job's exit comes back"
    );
    assert_eq!(
        fs::read_dir(s.path("remote-run")).unwrap().count(),
        0,
        "no script is left on the machine"
    );
}

#[test]
fn a_dead_callers_job_is_noticed_through_the_stream() {
    // Without the parent-death signal, the caller's death has to reach the far side as EOF on the
    // stream the script and command arrived on.
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let call = s
        .remote(s.dibs([
            "--label",
            "transport-hangup",
            &format!("{}; {}", up.signal(), never.hold()),
        ]))
        .env("DIBS_NO_PDEATHSIG", "1");
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
    let call = s.new_session([
        DIBS,
        "--label",
        "lease-asleep",
        &format!("{}; {}", up.signal(), never.hold()),
    ]);
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
    let out = s
        .remote(s.dibs(["--label", "lease-alive", &cmd]))
        .env("DIBS_LEASE", "1")
        .run();
    assert_eq!(out.stdout, "held\n");
}

#[test]
fn a_watch_whose_caller_stops_answering_stops_redrawing() {
    // A watch costs the machine a render on every tick, for nobody.
    let mut s = Sandbox::new();
    let call = s.new_session([DIBS, "--watch", "2"]);
    let watched = s.path("watched");
    let caller = s.spawn(s.remote(call).env("DIBS_LEASE", "2").stdout_to(&watched));
    let far = format!("^bash {}/.dibs-payload", s.p("remote-run"));
    let mut pid = 0;
    until("the watch to reach the machine", || {
        let out = s.command("pgrep", ["-f", &far]).run().stdout;
        pid = out
            .lines()
            .next()
            .and_then(|l| l.trim().parse().ok())
            .unwrap_or(0);
        pid != 0
    });
    signal_group(caller.pid, libc::SIGSTOP);
    let redrawing = || {
        let st = s
            .command("ps", ["-o", "stat=", "-p", &pid.to_string()])
            .run()
            .stdout;
        !matches!(st.trim().get(..1), None | Some("Z"))
    };
    until("the far side to stop redrawing", || !redrawing());
    signal_group(caller.pid, libc::SIGCONT);
    s.wait(caller);
    let redraws = s.read("watched").lines_with("ctrl-c to stop");
    assert!(
        (1..=4).contains(&redraws),
        "a watch nobody answers for stops within a lease or two of ticks, not after {redraws}"
    );
}

#[test]
fn a_heartbeat_is_not_a_tick() {
    // Every heartbeat woke the watch, so it redrew far more often than its interval: renders the
    // machine pays for, and ticks a reader cannot tell the interval from.
    let s = Sandbox::new();
    let out = s
        .remote(s.dibs(["--watch", "3", "--json"]))
        .env("DIBS_LEASE", "1")
        .within(Duration::from_secs(4))
        .run()
        .stdout;
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
    s.remote(s.dibs([
        "--sync",
        "-a",
        "--no-times",
        "--checksum",
        &format!("{}/", s.p("tsrc")),
        &format!(":{}/", s.p("tdst")),
    ]))
    .run();
    assert!(
        fs::read(s.path("tdst/sub/blob")).ok() == Some(blob.clone()),
        "a sync sends a file intact"
    );
    s.remote(s.dibs([
        "--sync",
        "-a",
        &format!(":{}/", s.p("tdst")),
        &format!("{}/", s.p("tback")),
    ]))
    .run();
    assert!(
        fs::read(s.path("tback/sub/blob")).ok() == Some(blob),
        "and fetches it back intact"
    );
}

#[test]
fn a_caller_that_goes_away_takes_the_whole_job_with_it() {
    // The channel closing is how the machine learns its caller is gone, and the lock is released as
    // soon as the job exits, so anything still running below the job would run unlocked.
    let mut s = Sandbox::new();
    let (ready, block) = (s.gate("ready"), s.gate("block"));
    s.write(
        "grand.sh",
        &format!(
            "echo $$ > {}\n{}\n{}\n",
            s.p("grand.pid"),
            ready.signal(),
            block.hold()
        ),
    );
    s.write(
        "mid.sh",
        &format!("sh {}\necho mid-done\n", s.p("grand.sh")),
    );
    let cmd = format!("bash {}; echo after", s.p("mid.sh"));
    let script = s.machine_script(&[("LABEL", "gone-caller"), ("CMD", &cmd)]);
    let (job, channel) = s.spawn_fed(s.command("bash", [script]));
    ready.reached();
    let grandchild: u32 = s.read("grand.pid").trim().parse().unwrap();
    assert!(alive(grandchild), "the job reached its grandchild");
    drop(channel);
    s.wait(job);
    assert!(
        !alive(grandchild),
        "the grandchild is gone once the job has ended"
    );
    assert_eq!(s.holders(), 0, "and the lock with it");
}

#[test]
fn a_transfer_still_preparing_is_stopped_when_its_session_ends() {
    // rsync owns the stream and reads none of it while a new tree is seeded, so nothing there sees
    // the caller go. The session the far half runs under does.
    let mut s = Sandbox::new();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let cmd = format!("{}; {}", up.signal(), never.hold());
    let script = s.machine_script(&[
        ("MODE", "rsh"),
        ("LABEL", "send-gone"),
        ("CMD", &cmd),
        ("NO_WATCH", "1"),
    ]);
    let (session, _channel) =
        s.spawn_fed(s.command("bash", ["-c", &format!("bash {script}; exit")]));
    up.reached();
    unsafe { libc::kill(session.pid as i32, libc::SIGKILL) };
    s.wait(session);
    s.log_line("caller-gone.*send-gone");
    s.gone();
}

#[test]
fn a_transfers_far_half_carries_its_stream_untouched() {
    // rsync's transport never reaches the machine from here, so the far half is run as rsync would.
    let s = Sandbox::new();
    let script = s.machine_script(&[
        ("MODE", "rsh"),
        ("LABEL", "sync"),
        ("CMD", "echo carried"),
        ("NO_WATCH", "1"),
    ]);
    let out = s.command("bash", [script]).run();
    assert_eq!(out.code, 0, "a transfer's far half exits with its command");
    assert_eq!(out.stdout, "carried\n", "and carries its stream untouched");
    assert_eq!(
        out.stderr.lines_with("unbound variable"),
        0,
        "with nothing unbound on the way out"
    );
}

#[test]
fn a_runners_transfer_carries_rsyncs_stream_untouched_once_it_says_so() {
    let s = Sandbox::new();
    let request = request_frame(Mode::Rsh, "sync", "cat");
    let out = s
        .dibs(["__runner", "serve"])
        .stdin(&format!("{request}rsync's own bytes\n"))
        .run();
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(
        out.stdout, "record 14\n\"transferring\"rsync's own bytes\n",
        "the runner says it read the request, and then frames nothing"
    );
    s.log_line("finished\t[0-9]+\trsh\tsync\t");
}

#[test]
fn a_runners_transfer_still_preparing_is_stopped_when_its_reader_goes() {
    let mut s = Sandbox::new();
    let (up, never, go) = (s.gate("up"), s.gate("never"), s.gate("go"));
    let command = format!("{}; {}", up.signal(), never.hold());
    s.write("request", &request_frame(Mode::Rsh, "send-gone", &command));
    let job = s.spawn(s.sh(&format!(
        "dibs __runner serve < {} | {{ head -c 1 >/dev/null; {}; }}",
        s.p("request"),
        go.hold()
    )));
    up.reached();
    s.held(1);
    go.open();
    s.wait(job);
    s.log_line("caller-gone.*send-gone");
    s.gone();
}
