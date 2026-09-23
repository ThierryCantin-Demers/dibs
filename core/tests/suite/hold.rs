use crate::harness::*;
use std::fs;

/// A sandbox whose TMPDIR is its own, so that a hold leaving something behind there shows.
fn holding() -> Sandbox {
    let mut s = Sandbox::new();
    fs::create_dir_all(s.path("htmp")).unwrap();
    s.set("TMPDIR", s.p("htmp"));
    s
}

fn left_in_tmpdir(s: &Sandbox) -> usize {
    fs::read_dir(s.path("htmp")).unwrap().count()
}

#[test]
fn a_hold_runs_its_command_here_under_the_lock() {
    let s = holding();
    assert_eq!(s.dibs(["--hold", "--label", "hold-exit", "exit 3"]).code(), 3, "a hold exits with its command's status");
    assert_eq!(
        s.dibs(["--hold", "--label", "hold-where", r#"echo "${DIBS_JOB:-here}""#]).run().stdout,
        "here\n",
        "the command runs here, not inside the job"
    );
    assert_eq!(
        s.dibs(["--bench", "--hold", "--label", "hold-in", r#"cut -f1,4 "$DIBS_LOCK_DIR"/holder.*"#]).run().stdout,
        "bench\thold-in\n",
        "under the lock"
    );
    assert_eq!(s.holders(), 0, "which goes when it ends");
    let finished: Vec<String> = s
        .log()
        .lines()
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|f| f.get(1) == Some(&"finished") && f.get(4) == Some(&"hold-exit"))
        .map(|f| format!("{}: {}", f[7], f[8]))
        .collect();
    assert_eq!(finished, ["3: held for a command run elsewhere: exit 3"], "the log has what was held and how it ended");
    assert_eq!(s.dibs(["--hold", "--label", "hold-words", "printf", "%s|", "a", "b c"]).run().stdout, "a|b c|", "several words stay several words");
    assert_eq!(
        s.dibs(["--hold", "--label", "hold-stdin", r#"read -r x; echo "$x""#]).stdin("in\n").run().stdout,
        "in\n",
        "the command keeps stdin"
    );
    s.dibs(["--bench", "--hold", "--label", "hold-series", "true"]).run();
    assert_eq!(s.read("series").lines().filter(|l| l.starts_with("hold-series\t")).count(), 0, "a bench hold starts no series, since it measured nothing there");
    assert_eq!(left_in_tmpdir(&s), 0, "a hold leaves nothing in TMPDIR");
}

#[test]
fn status_does_not_call_a_hold_idle() {
    let mut s = holding();
    let (up, go) = (s.gate("up"), s.gate("go"));
    let hold = s.spawn(s.dibs(["--hold", "--label", "hold-status", &format!("{}; {}", up.signal(), go.hold())]));
    up.reached();
    s.status();
    assert_eq!(s.dibs(["--status"]).env("DIBS_IDLE_AFTER", "-1").run().stdout.lines_with("IDLE"), 0, "status does not call a hold idle");
    assert!(!s.dibs(["--status", "--json"]).env("DIBS_IDLE_AFTER", "-1").run().stdout.contains("idle_for"), "nor does its JSON");
    go.open();
    s.wait(hold);
}

#[test]
fn a_lock_that_goes_first_ends_the_hold_and_its_command() {
    let s = holding();
    let never = s.gate("never");
    let cmd = format!("echo $$ > {}; {}", s.p("hold-m.pid"), never.hold());
    assert_eq!(s.dibs(["--hold", "--max", "1", "--label", "hold-max", &cmd]).code(), 124, "a lock that goes first ends the hold");
    let pid: u32 = s.read("hold-m.pid").trim().parse().unwrap();
    assert!(!alive(pid), "and stops the command, which would otherwise run on unlocked");
}

#[test]
fn a_hold_busy_past_its_wait_never_runs_its_command() {
    let mut s = holding();
    let (up, go) = (s.gate("up"), s.gate("go"));
    let blocker = s.spawn(s.dibs(["--bench", "--label", "hold-blocker", &format!("{}; {}", up.signal(), go.hold())]));
    up.reached();
    let code = s.dibs(["--hold", "--wait", "1", "--label", "hold-busy", &format!("touch {}", s.p("hold-busy-ran"))]).code();
    assert_eq!((code, s.exists("hold-busy-ran")), (75, false));
    go.open();
    s.wait(blocker);
}

#[test]
fn what_a_hold_cannot_mean_is_refused() {
    let s = holding();
    assert_eq!(s.dibs(["--peek", "--hold", "true"]).code(), 2, "a peek holds nothing, so it cannot hold");
    assert_eq!(s.dibs(["--hold", "--device", "gpu:x", "true"]).code(), 2, "and a card there is nothing a command here could use");
}

#[test]
fn over_the_transport_a_hold_still_runs_its_command_here() {
    let s = holding();
    assert_eq!(
        s.remote(s.dibs(["--hold", "--label", "hold-remote", r#"echo "${DIBS_JOB:-here}""#])).run().stdout,
        "here\n",
        "over the transport, the command runs here"
    );
    assert_eq!(s.remote(s.dibs(["--bench", "--hold", "--label", "hold-remote", "exit 4"])).code(), 4, "and its exit comes back");
    assert_eq!(left_in_tmpdir(&s), 0, "a hold leaves nothing in TMPDIR");
}

#[test]
fn a_lock_inside_a_hold_of_the_same_machine_is_refused() {
    // Left waiting, it would wait on the hold around it for ever.
    let s = holding();
    let inner = format!("{DIBS} --label hold-inner true");
    assert_eq!(s.dibs(["--hold", "--max", "20", "--label", "hold-nest", &inner]).code(), 2, "a lock inside a hold of the same machine is refused");
    assert_eq!(s.dibs(["--hold", "--label", "hold-nest", &format!("{DIBS} --peek true")]).code(), 0, "while a peek there still runs");
    let elsewhere = format!("DIBS_LOCAL=1 {inner}");
    assert_eq!(s.remote(s.dibs(["--hold", "--label", "hold-nest", &elsewhere])).code(), 0, "and so does a lock on another machine");
}

#[test]
fn a_batch_step_can_hold() {
    let s = holding();
    let step = format!("[h] dibs --hold --label hold-batch 'echo ${{DIBS_JOB:-here}} > {}'\n", s.p("hold-batch"));
    let code = s.dibs(["batch", "-"]).stdin(&step).code();
    assert_eq!((code, s.read("hold-batch")), (0, "here\n".to_string()));
    assert_eq!(left_in_tmpdir(&s), 0, "a hold leaves nothing in TMPDIR");
}

#[test]
fn a_hold_whose_caller_died_is_let_go_with_its_command() {
    let mut s = holding();
    let (up, never) = (s.gate("up"), s.gate("never"));
    let cmd = format!("echo $$ > {}; {}; {}", s.p("hold-k.pid"), up.signal(), never.hold());
    let caller = s.spawn(s.remote(s.dibs(["--hold", "--label", "hold-gone", &cmd])));
    up.reached();
    unsafe { libc::kill(caller.pid as i32, libc::SIGKILL) };
    s.wait(caller);
    s.log_line("caller-gone.*hold-gone");
    s.gone();
    let pid: u32 = s.read("hold-k.pid").trim().parse().unwrap();
    until("the held command to stop", || !alive(pid));
}
