use crate::harness::*;

fn fake_core(s: &Sandbox) -> String {
    s.write_exec("fakecore", "#!/bin/sh\necho \"core $*\"\n");
    s.p("fakecore")
}

#[test]
fn run_status_and_out_are_words_as_well_as_flags() {
    let s = Sandbox::new();
    assert_eq!(s.dibs(["run", "--label", "one-run", "echo via-run"]).run().stdout, "via-run\n", "run is the bare form");
    assert_eq!(s.dibs(["status"]).run().stdout.lines_with("dibs: idle"), 1, "status is --status");
    let job = job_id(&s.dibs(["--label", "one-out", "echo kept"]).run().stderr);
    assert_eq!(s.dibs(["out", &job]).run().all().lines_with("| kept"), 1, "out is --out");
    assert_eq!(s.dibs(["--label", "one-run2", "run", "echo after-flags"]).run().stdout, "after-flags\n", "a subcommand may follow flags");
    assert_eq!(s.dibs(["-v", "status"]).run().stdout.lines_with("dibs: idle"), 1, "status too");
    assert_eq!(s.dibs(["run", "echo", "status"]).run().stdout, "status\n", "a word after the command is the command's");
}

#[test]
fn a_recipe_verb_goes_to_the_recipe_layer_with_its_arguments_intact() {
    let s = Sandbox::new();
    let core = fake_core(&s);
    let via = |args: &[&str]| s.dibs(args).env("DIBS_CORE", &core).run();
    assert_eq!(via(&["bench", "cubek@local", "gemm", "--dry-run"]).stdout, "core bench cubek@local gemm --dry-run\n");
    assert_eq!(via(&["list", "cubek"]).stdout, "core list cubek\n", "and list too");
    assert_eq!(via(&["--label", "x", "list", "cubek"]).code, 2, "a flag other than --on before a recipe verb is refused");
    assert_eq!(
        s.dibs(["list", "cubek"]).env("DIBS_CORE", "").env("HOME", s.p("nohome")).code(),
        2,
        "a missing recipe layer is refused"
    );
}
