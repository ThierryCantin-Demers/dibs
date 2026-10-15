//! A batch driven on its machine, so it outlives this computer: what is sent, what comes back,
//! and what is refused before anything is.

use crate::harness::*;
use crate::prepares::short_hash;
use crate::recipes::{PARAMS, app, recipes};
use crate::wire::wired;
use std::fs;

const CLIENT_HASH: &str = env!("DIBS_CLIENT_HASH");

/// box-a's far side run here, where this build's dibs stands in for the one built there.
fn detaching() -> Sandbox {
    let mut s = wired();
    s.set("WIRE_RUN", "1");
    s.set("WIRE_HOSTS", s.p("hosts"));
    s.set("DIBS_ROOT", s.p("."));
    s.write_exec(
        &format!("home/.cache/dibs/client/{CLIENT_HASH}/dibs"),
        &format!("#!/bin/sh\nexec '{DIBS}' \"$@\"\n"),
    );
    s
}

const STEPS: &str = "[one] dibs build app@local p --on box-a\n\
                     [two] dibs --on box-a --label detached-two 'echo second'\n";

#[test]
fn a_detached_batch_is_driven_on_its_machine_and_its_summary_comes_home() {
    let s = detaching();
    let dir = app(&s);
    recipes(&s, PARAMS);
    s.write("app/uncommitted.txt", "sent as it is\n");
    let out = s.dibs(["batch", "--detach", "-"]).stdin(STEPS).run();
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(
        out.stdout.lines_matching("^batch [0-9-]*  2 steps, "),
        1,
        "{}",
        out.all()
    );
    assert_eq!(out.stdout.lines_with("its jobs ran on box-a"), 1);
    assert!(
        s.read("home/.local/state/dibs/runs.jsonl")
            .contains("\"repo\":\"app\""),
        "the recipe's run record is filed here"
    );
    assert_eq!(
        s.read("home/.cache/dibs/detached/last/app/uncommitted.txt"),
        "sent as it is\n",
        "the working tree went whole, and is kept for the next batch to be sent against"
    );
    assert!(
        s.path(&format!(
            "hosts/box-a/scratch/ws/app/local-{}",
            short_hash(&dir)
        ))
        .is_dir(),
        "the tree sent on there is keyed as this computer's own calls key it"
    );
}

#[test]
fn a_follow_cut_off_says_how_to_follow_again() {
    let mut s = detaching();
    app(&s);
    recipes(&s, PARAMS);
    s.write_exec(
        "cut/ssh",
        &format!(
            "#!/bin/bash\ncase \"$*\" in *'__detached follow'*) [ -e '{cut}' ] || {{ : > '{cut}'; echo 'Connection closed by remote host' >&2; exit 255; }} ;; esac\nexec '{ssh}' \"$@\"\n",
            cut = s.p("cut/once"),
            ssh = s.p("wirebin/ssh"),
        ),
    );
    s.set("PATH", format!("{}:{}", s.p("cut"), s.var("PATH")));
    let out = s.dibs(["batch", "--detach", "-"]).stdin(STEPS).run();
    assert_eq!(out.code, 69, "{}", out.all());
    let id = out
        .stderr
        .lines()
        .find_map(|l| l.trim().strip_prefix("dibs batch --follow "))
        .unwrap_or_else(|| panic!("no follow line:\n{}", out.all()))
        .to_string();
    let again = s.dibs(["batch", "--follow", &id]).run();
    assert_eq!(again.code, 0, "{}", again.all());
    assert_eq!(
        again
            .stdout
            .lines_matching(&format!("^batch {id}  2 steps, ")),
        1,
        "{}",
        again.all()
    );
}

#[test]
fn a_batch_that_needs_this_computer_is_refused_before_anything_is_sent() {
    let s = detaching();
    app(&s);
    recipes(&s, PARAMS);
    for (steps, says) in [
        (
            "[a] dibs --on box-a true\n[b] dibs --on box-b true\n",
            "driven on\n  its one machine",
        ),
        ("[a] dibs --label x true\n", "step a names no machine"),
        (
            "[a] dibs --on box-a --sync ./x :~/y\n",
            "copies between this computer and the machine",
        ),
        (
            "[a] dibs with app svc --on box-a -- true\n",
            "--there runs it on the machine",
        ),
        (
            "[a] dibs build ./app@local p --on box-a\n",
            "names ./app by a path",
        ),
    ] {
        let out = s.dibs(["batch", "--detach", "-"]).stdin(steps).run();
        assert_eq!(out.code, 2, "{steps}{}", out.all());
        assert!(out.stderr.contains(says), "{steps}{}", out.all());
    }
    assert!(
        !s.path("home/.cache/dibs/detached").exists(),
        "nothing was sent"
    );
}

#[test]
fn a_detached_batch_is_stopped_from_here_on_its_machine() {
    let mut s = detaching();
    let gate = s.gate("held");
    let steps = format!(
        "[held] dibs --on box-a --label detached-held '{}'\n[after] dibs --on box-a --label detached-after true\n",
        gate.hold()
    );
    s.write("steps", &steps);
    let job = s.spawn(s.dibs(["batch", "--detach", &s.p("steps")]));
    let holders = || {
        fs::read_dir(s.path("hosts/box-a/lock"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("holder."))
            .count()
    };
    s.until_records("the held step to run on box-a", || holders() == 1);
    let id = fs::read_dir(s.path("home/.local/state/dibs/batch"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .next()
        .unwrap();
    let out = s.dibs(["--kill", &id]).run();
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(
        out.all().lines_with(&format!(
            "batch {id} is driven on box-a, so it is stopped there"
        )),
        1,
        "{}",
        out.all()
    );
    assert_eq!(
        s.wait(job),
        76,
        "the driver there stops, and its follow here with it"
    );
}
