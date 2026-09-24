use crate::harness::*;

#[test]
fn a_malformed_sync_is_refused_before_anything_is_reached_for() {
    let s = Sandbox::new();
    assert_eq!(s.dibs(["--sync", "./a", "./b"]).code(), 2, "--sync wants the machine's side marked");
    assert_eq!(s.dibs(["--sync", ":~/x"]).code(), 2, "and it wants two paths");
    assert_eq!(s.dibs(["--rsh"]).code(), 2, "--rsh is not for hands");
    // Everything after --sync is rsync's, so a dibs flag there would reach rsync as a path.
    assert_eq!(s.dibs(["--sync", "--on", "x", "./a", ":~/b"]).code(), 2, "a dibs flag after --sync is refused");
    assert_eq!(s.dibs(["--sync", "--label", "y", "./a", ":~/b"]).run().all().lines_with("Put it before"), 1, "and told where it goes");
}

#[test]
fn a_variable_on_the_machine_side_is_refused_rather_than_sent_literally() {
    // Sent as is, it failed on the machine and came back as 69, which reads as the machine down.
    let s = Sandbox::new();
    for dst in [":$DIBS_SCRATCH/x", ":${DIBS_SCRATCH}/x", ":$HOME/x", ":~/a/$TMPDIR"] {
        let out = s.dibs(["--sync", "./x", dst]).run();
        assert_eq!((out.code, out.all().lines_with("does not expand variables")), (2, 1), "{dst}");
    }
    assert_eq!(s.dibs(["--sync", "./$x", ":~/x"]).run().all().lines_with("does not expand"), 0, "a $ on this side is the shell's business");
}

#[test]
fn preserving_mtimes_into_the_machine_is_warned_about() {
    // A build after a sync that kept mtimes compiles nothing.
    let s = Sandbox::new();
    let warned = |args: &[&str]| s.dibs(args).run().all().lines_with("preserving mtimes");
    assert_eq!(warned(&["--sync", "-a", "./a", ":~/b"]), 1, "preserving mtimes into the machine is warned about");
    assert_eq!(warned(&["--sync", "-a", ":~/b", "./a"]), 0, "but not when fetching");
    assert_eq!(warned(&["--sync", "-a", "--no-times", "--checksum", "./a", ":~/b"]), 0, "nor when times are turned off");
}

#[test]
fn a_sync_to_the_machine_you_are_on_copies_rather_than_refusing() {
    // The caller that cannot take advice to use cp is a program: a recipe sending a local
    // worktree to a machine that is this one.
    let s = Sandbox::new();
    s.write("syncsrc/f.txt", "carried\n");
    let (src, dst) = (format!("{}/", s.p("syncsrc")), format!(":{}/", s.p("syncdst")));
    let out = s.dibs(["--sync", "-rlpgo", &src, &dst]).run();
    assert_eq!(out.code, 0, "it succeeds");
    assert_eq!(s.read("syncdst/f.txt"), "carried\n", "and the file is there");
    assert_eq!(out.all().lines_with("use cp"), 0, "and it did not tell a program to use cp");
    assert_eq!(out.all().lines_with("You are on it"), 0);
    s.dibs(["--sync", "-rlpgo", &src, &format!(":{}/", s.p("syncnest/a/b"))]).run();
    assert_eq!(s.read("syncnest/a/b/f.txt"), "carried\n", "a destination whose parents do not exist yet is created");
}

#[test]
fn a_transfer_goes_where_it_was_told() {
    // rsync reaches the machine through a second dibs that never saw --on, so the resolved
    // machine rides in the environment the child inherits.
    let mut s = Sandbox::new();
    s.machines("[machine.wrongbox]\nssh      = \"dibs@wrongbox\"\nhostname = \"wrongbox\"\n\n[machine.rightbox]\nssh      = \"dibs@rightbox\"\nhostname = \"rightbox\"\n");
    let out = s.dibs(["--on", "rightbox", "--sync", "./x", ":~/y"]).env("DIBS_LOCAL", "0").env("DIBS_CONNECT_TIMEOUT", "2").run().all();
    assert!(out.contains("dibs@rightbox"), "--sync carries --on to the transport it spawns");
    assert_eq!(out.lines_with("wrongbox"), 0, "and does not fall back to another machine");
    // A transfer to the wrong machine succeeds, so the only moment to catch it is before.
    assert_eq!(out.lines_with("syncing with dibs@rightbox"), 1, "and says where it is about to write");
}
