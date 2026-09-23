use crate::harness::*;
use std::fs;

/// A clone of dibs with a stub installer, and a clone of recipes, each one commit behind its origin.
struct Clones {
    head: String,
}

fn clones(s: &Sandbox) -> Clones {
    fs::create_dir_all(s.path("update/bin")).unwrap();
    s.git(".", &["init", "-q", "-b", "main", "update/origin"]);
    fs::create_dir_all(s.path("update/origin/bin")).unwrap();
    fs::copy(DIBS, s.path("update/origin/bin/dibs")).unwrap();
    s.command("cp", ["-r", &repo_root().join("lib").display().to_string(), &s.p("update/origin/lib")]).run();
    s.write("update/origin/install.sh", &format!("echo installed >> {}\n", s.p("update/installs")));
    s.git("update/origin", &["add", "-A"]);
    s.git("update/origin", &["commit", "-qm", "one"]);
    s.git(".", &["clone", "-q", "update/origin", "update/clone"]);
    s.git(".", &["init", "-q", "-b", "main", "update/rorigin"]);
    s.write("update/rorigin/r.toml", "[build.x]\n");
    s.git("update/rorigin", &["add", "-A"]);
    s.git("update/rorigin", &["commit", "-qm", "r1"]);
    s.git(".", &["clone", "-q", "update/rorigin", "update/recipes"]);
    commit(s, "update/origin", "two");
    commit(s, "update/rorigin", "r2");
    let head = s.git("update/origin", &["rev-parse", "--short", "HEAD"]);
    installed_core(s, &head);
    Clones { head }
}

fn commit(s: &Sandbox, repo: &str, name: &str) {
    s.write(&format!("{repo}/{name}"), &format!("{name}\n"));
    s.git(repo, &["add", "-A"]);
    s.git(repo, &["commit", "-qm", name]);
}

/// The recipe layer as installed, reporting the commit it was built from.
fn installed_core(s: &Sandbox, head: &str) {
    s.write_exec("update/bin/dibs-core", &format!("#!/bin/sh\necho \"dibs-core 0.1.0 ({head})\"\n"));
}

fn update(s: &Sandbox, recipes: &str) -> Output {
    let clone = s.p("update/clone/bin/dibs");
    s.command(&clone, ["--update"])
        .env("PATH", format!("{}:{}", s.p("update/bin"), s.var("PATH")))
        .env("DIBS_CORE", s.p("update/bin/dibs-core"))
        .env("DIBS_RECIPES", s.p(recipes))
        .run()
}

#[test]
fn an_update_pulls_reinstalls_and_says_what_arrived() {
    let s = Sandbox::new();
    let c = clones(&s);
    let out = update(&s, "update/recipes");
    assert_eq!(out.code, 0, "an update succeeds: {}", out.all());
    assert_eq!(s.git("update/clone", &["rev-parse", "--short", "HEAD"]), c.head, "it fast-forwards its own clone");
    assert_eq!(out.all().lines_matching("^  [0-9a-f]* two$"), 1, "and names what arrived");
    assert_eq!(s.read("update/installs").lines().count(), 1, "and reinstalls");
    assert_eq!(
        s.git("update/recipes", &["rev-parse", "HEAD"]),
        s.git("update/rorigin", &["rev-parse", "HEAD"]),
        "and pulls the recipes"
    );
    let again = update(&s, "update/recipes");
    assert_eq!(s.read("update/installs").lines().count(), 1, "a current install is not rebuilt");
    assert_eq!(again.all().lines_with("already current"), 2, "and says it is current");
    installed_core(&s, "0000000");
    update(&s, "update/recipes");
    assert_eq!(s.read("update/installs").lines().count(), 2, "a stale recipe layer is rebuilt even with nothing to pull");
    assert_eq!(update(&s, "update/nowhere").all().lines_with("recipes"), 0, "recipes that are not a clone are left alone quietly");
}

#[test]
fn a_copy_outside_a_clone_is_refused_but_still_runs() {
    // Laid out the way install.sh --copy lays it out, so what is refused is the update and not a
    // missing lib.
    let mut s = Sandbox::new();
    s.machines("[machine.m]\nssh = \"m\"\nhostname = \"m\"\n");
    fs::create_dir_all(s.path("loose/bin")).unwrap();
    fs::create_dir_all(s.path("loose/libexec/dibs")).unwrap();
    fs::copy(DIBS, s.path("loose/bin/dibs")).unwrap();
    s.command("cp", ["-r", &repo_root().join("lib").display().to_string(), &s.p("loose/libexec/dibs/lib")]).run();
    let copy = s.p("loose/bin/dibs");
    let out = s.command(&copy, ["--update"]).run();
    assert_eq!(out.code, 2, "a copy outside a clone is refused");
    assert_eq!(out.all().lines_with("not inside a git clone"), 1, "because it has no clone to pull");
    assert_eq!(s.command(&copy, ["--machines"]).code(), 0, "a copy finds its lib under libexec");
}

#[test]
fn a_session_is_told_once_when_dibs_changed_under_it() {
    let s = Sandbox::new();
    clones(&s);
    let clone = s.p("update/clone/bin/dibs");
    let as_session = |id: &str, args: &[&str]| {
        s.command(&clone, args)
            .env_remove("CLAUDE_CODE_HOST_SESSION_ID")
            .env("CLAUDE_CODE_SESSION_ID", id)
            .env("DIBS_SEEN", s.p("seen-v"))
            .env("DIBS_CORE", s.p("fakecore"))
            .env("DIBS_RECIPES", s.p("update/nowhere"))
            .run()
            .stderr
    };
    s.write_exec("fakecore", "#!/bin/sh\necho \"core $*\"\n");
    let pulled = |name: &str| {
        commit(&s, "update/origin", name);
        s.git("update/clone", &["pull", "-q", "--ff-only"]);
    };
    assert_eq!(as_session("v1", &["--status"]).lines_with("dibs changed"), 0, "a first call says nothing");
    pulled("three");
    let told = as_session("v1", &["--status"]);
    assert_eq!(told.lines_with("dibs changed since this session last ran it: "), 1, "the next call after a change says so");
    assert_eq!(told.lines_matching("^  [0-9a-f]* three$"), 1, "and lists what changed");
    assert_eq!(as_session("v1", &["--status"]).lines_with("dibs changed"), 0, "once");
    assert_eq!(as_session("v2", &["--status"]).lines_with("dibs changed"), 0, "a session that never saw the old one is not told");
    pulled("four");
    assert_eq!(as_session("v1", &["list", "x"]).lines_with("dibs changed"), 1, "a recipe verb is told too");
    commit(&s, "update/origin", "five");
    as_session("v1", &["--update"]);
    assert_eq!(as_session("v1", &["--status"]).lines_with("dibs changed"), 0, "an update it ran itself is not reported again");
}
