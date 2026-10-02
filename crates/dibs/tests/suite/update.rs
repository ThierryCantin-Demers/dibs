//! `dibs --update` and the change notice. Both act on the clone the binary was built from, so
//! they are driven here through the library against a clone in the sandbox, and only the notice
//! a call prints is driven through the binary.

use crate::harness::*;
use crate::snapshot::*;
use dibs::{
    caller::Caller,
    update::{ChangeNotice, Update},
};
use std::{
    fs, iter,
    os::unix::fs::{PermissionsExt as _, symlink},
};

/// A clone of dibs with a stub installer, and a clone of recipes, each one commit behind its origin.
struct Clones {
    head: String,
}

fn clones(s: &Sandbox) -> Clones {
    s.git(".", &["init", "-q", "-b", "main", "update/origin"]);
    s.write("update/origin/dibs-agent-rules.md", "rules\n");
    s.write(
        "update/origin/install.sh",
        &format!("echo installed >> {}\n", s.p("update/installs")),
    );
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
    Clones {
        head: s.git("update/origin", &["rev-parse", "--short", "HEAD"]),
    }
}

fn commit(s: &Sandbox, repo: &str, name: &str) {
    s.write(&format!("{repo}/{name}"), &format!("{name}\n"));
    s.git(repo, &["add", "-A"]);
    s.git(repo, &["commit", "-qm", name]);
}

fn session(id: &str) -> Caller {
    Caller {
        id: id.into(),
        name: format!("session {id}"),
    }
}

fn notice(s: &Sandbox, seen: &str) -> ChangeNotice {
    ChangeNotice {
        seen: s.path(seen),
        clone: s.path("update/clone"),
    }
}

/// `dibs --update` from a build stamped `installed`, in the session `caller`.
fn update(s: &Sandbox, installed: &str, recipes: &str, caller: &str) -> Output {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = Update {
        clone: s.path("update/clone"),
        installed: Some(installed.into()),
        recipes: Some(s.path(recipes)),
        notice: Some(notice(s, "seen")),
        caller: session(caller),
        prefix: None,
    }
    .run_into(&mut out, &mut err);
    Output {
        code,
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
    }
}

/// What a build stamped with the clone's head tells `caller`.
fn told(s: &Sandbox, notice: &ChangeNotice, caller: &str) -> String {
    let head = s.git("update/clone", &["rev-parse", "--short", "HEAD"]);
    notice.record(&session(caller), &head).unwrap_or_default()
}

fn pulled(s: &Sandbox, name: &str) {
    commit(s, "update/origin", name);
    s.git("update/clone", &["pull", "-q", "--ff-only"]);
}

#[test]
fn an_update_pulls_reinstalls_and_says_what_arrived() {
    let s = Sandbox::new();
    let c = clones(&s);
    let out = update(&s, &c.head, "update/recipes", "u");
    assert_eq!(out.code, 0, "an update succeeds: {}", out.all());
    assert_eq!(
        s.git("update/clone", &["rev-parse", "--short", "HEAD"]),
        c.head,
        "it fast-forwards its own clone"
    );
    assert_eq!(
        out.all().lines_matching("^  [0-9a-f]* two$"),
        1,
        "and names what arrived"
    );
    assert_eq!(
        s.read("update/installs").lines().count(),
        1,
        "and reinstalls"
    );
    assert_eq!(
        s.git("update/recipes", &["rev-parse", "HEAD"]),
        s.git("update/rorigin", &["rev-parse", "HEAD"]),
        "and pulls the recipes"
    );
    let again = update(&s, &c.head, "update/recipes", "u");
    assert_eq!(
        s.read("update/installs").lines().count(),
        1,
        "a current install is not rebuilt"
    );
    assert_eq!(
        again.all().lines_with("already current"),
        2,
        "and says it is current"
    );
    update(&s, "0000000", "update/recipes", "u");
    assert_eq!(
        s.read("update/installs").lines().count(),
        2,
        "a build from another commit is rebuilt even with nothing to pull"
    );
    assert_eq!(
        update(&s, &c.head, "update/nowhere", "u")
            .all()
            .lines_with("recipes"),
        0,
        "recipes that are not a clone are left alone quietly"
    );
}

#[test]
fn an_update_outside_a_clone_is_refused() {
    let s = Sandbox::new();
    fs::create_dir_all(s.path("loose")).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = Update {
        clone: s.path("loose"),
        installed: None,
        recipes: None,
        notice: None,
        caller: session("u"),
        prefix: None,
    }
    .run_into(&mut out, &mut err);
    assert_eq!(code, 2, "a build from no clone has nothing to pull");
    assert_eq!(
        String::from_utf8_lossy(&err).lines_with("not inside a git clone"),
        1,
        "and says so"
    );
}

#[test]
fn a_session_is_told_once_when_dibs_changed_under_it() {
    let s = Sandbox::new();
    let c = clones(&s);
    let seen = notice(&s, "seen-v");
    assert_eq!(told(&s, &seen, "v1"), "", "a first call says nothing");
    pulled(&s, "three");
    let text = told(&s, &seen, "v1");
    assert_eq!(
        text.lines_with("dibs changed since this session last ran it: "),
        1,
        "the next call after a change says so"
    );
    assert_eq!(
        text.lines_matching("^  [0-9a-f]* three$"),
        1,
        "and lists what changed"
    );
    assert_eq!(told(&s, &seen, "v1"), "", "once");
    assert_eq!(
        told(&s, &seen, "v2"),
        "",
        "a session that never saw the old one is not told"
    );
    commit(&s, "update/origin", "four");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    Update {
        clone: s.path("update/clone"),
        installed: Some(c.head),
        recipes: None,
        notice: Some(notice(&s, "seen-v")),
        caller: session("v1"),
        prefix: None,
    }
    .run_into(&mut out, &mut err);
    assert_eq!(
        told(&s, &seen, "v1"),
        "",
        "an update it ran itself is not reported again"
    );
}

/// The binary's version is the commit stamped into it, so a stamp from no commit at all stands in
/// for an old one.
#[test]
fn a_call_and_a_recipe_verb_tell_their_session() {
    let s = Sandbox::new();
    let stamped = |s: &Sandbox| {
        for stamp in fs::read_dir(s.path("seen")).unwrap().flatten() {
            fs::write(stamp.path(), "0000000\n").unwrap();
        }
    };
    s.dibs(["--status"]).run();
    stamped(&s);
    assert_eq!(
        s.dibs(["--status"])
            .run()
            .stderr
            .lines_with("dibs changed since this session last ran it: 0000000 -> "),
        1,
        "a call after a change says so"
    );
    assert_eq!(
        s.dibs(["--status"]).run().stderr.lines_with("dibs changed"),
        0,
        "once"
    );
    stamped(&s);
    assert_eq!(
        s.dibs(["list", "x"])
            .run()
            .stderr
            .lines_with("dibs changed"),
        1,
        "a recipe verb is told too"
    );
}

#[test]
fn what_an_update_and_the_change_notice_print() {
    let s = Sandbox::new();
    let c = clones(&s);
    let n = Normal::of(&s).rule(r"\b[0-9a-f]{7,12}\b", "<sha>");
    let notice = notice(&s, "seen-n");
    let said = |text: String| Output {
        code: 0,
        stdout: String::new(),
        stderr: text,
    };
    let mut t = Transcript::default();
    told(&s, &notice, "n");
    t.section(
        "dibs --update  (one commit behind, and the recipes one behind theirs)",
        &n.output(&update(&s, &c.head, "update/recipes", "u")),
    );
    t.section(
        "the change notice  (in a session that last ran it before that update)",
        &n.output(&said(told(&s, &notice, "n"))),
    );
    for i in 0..12 {
        commit(&s, "update/origin", &format!("more-{i:02}"));
    }
    s.git("update/clone", &["pull", "-q", "--ff-only"]);
    t.section(
        "the change notice  (after twelve more commits arrived)",
        &n.output(&said(told(&s, &notice, "n"))),
    );
    let head = s.git("update/clone", &["rev-parse", "--short", "HEAD"]);
    t.section(
        "dibs --update  (with nothing to pull)",
        &n.output(&update(&s, &head, "update/recipes", "u")),
    );
    t.section(
        "dibs --update  (with recipes that are not a clone)",
        &n.output(&update(&s, &head, "update", "u")),
    );
    snapshot("update", t.text());
}

/// This build's binary with every mention of its clone pointed at a path that does not exist, as
/// a build is once its clone has moved or been deleted.
fn with_its_clone_gone(s: &Sandbox) -> String {
    let mut bytes = fs::read(DIBS).unwrap();
    let named = repo_root();
    for clone in [named.clone(), named.canonicalize().unwrap()] {
        let clone = clone.display().to_string().into_bytes();
        let gone: Vec<u8> = iter::once(b'/')
            .chain(iter::repeat_n(b'x', clone.len() - 1))
            .collect();
        let mut at = 0;
        while let Some(found) = bytes[at..].windows(clone.len()).position(|w| w == clone) {
            let start = at + found;
            bytes[start..start + clone.len()].copy_from_slice(&gone);
            at = start + clone.len();
        }
    }
    let copy = s.path("moved/dibs");
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::write(&copy, bytes).unwrap();
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o755)).unwrap();
    if cfg!(target_os = "macos") {
        let signed = s.command("codesign", ["--force", "-s", "-", &s.p("moved/dibs")]);
        assert_eq!(signed.code(), 0, "a patched binary has to be signed again");
    }
    fs::remove_file(s.path("bin/dibs")).unwrap();
    symlink(&copy, s.path("bin/dibs")).unwrap();
    s.p("moved/dibs")
}

#[test]
fn a_build_keeps_working_once_its_clone_is_gone() {
    let s = Sandbox::new();
    let dibs = with_its_clone_gone(&s);
    let ran = s
        .command(&dibs, ["--label", "gone", "echo", "it ran"])
        .run();
    assert_eq!(
        (ran.code, ran.stdout.lines_with("it ran")),
        (0, 1),
        "a call runs on the machine half it was built with: {}",
        ran.all()
    );
    for words in [["--status"].as_slice(), &["runs"], &["--version"]] {
        let out = s.command(&dibs, words).run();
        assert_eq!(out.code, 0, "{words:?}: {}", out.all());
    }
}
