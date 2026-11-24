use crate::harness::*;
use std::fs;

fn ran(s: &Sandbox) -> usize {
    fs::read_to_string(s.path("scratch").join("ran"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn an_attempt_is_run_once_and_asked_again_says_how_it_ended() {
    let s = Sandbox::new();
    let trailers = s.path("trailers");
    let call = || {
        s.dibs([
            "--label",
            "attempted",
            r#"echo ran >> "$DIBS_SCRATCH/ran"; exit 4"#,
        ])
        .env("DIBS_ATTEMPT", "sub.1.1")
        .env("DIBS_TRAILERS", trailers.to_str().unwrap())
        .run()
    };
    let first = call();
    assert_eq!(first.code, 4, "{}", first.all());
    let again = call();
    assert_eq!(again.code, 4, "{}", again.all());
    assert_eq!(
        again.stderr.lines_with("is not run again"),
        1,
        "{}",
        again.all()
    );
    assert_eq!(ran(&s), 1);
    let noted = fs::read_to_string(&trailers).unwrap();
    let kinds: Vec<&str> = noted
        .lines()
        .map(|l| match l {
            l if l.starts_with(r#"{"trailer""#) => "trailer",
            l if l.starts_with(r#"{"admitted":{"ended""#) => "ended",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["trailer", "ended"], "{noted}");
}

#[test]
fn an_attempt_still_running_is_not_started_again() {
    let mut s = Sandbox::new();
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let script = format!(
        r#"echo ran >> "$DIBS_SCRATCH/ran"; {}; {}"#,
        up.signal(),
        hold.hold()
    );
    let first = s
        .dibs(["--label", "attempted", &script])
        .env("DIBS_ATTEMPT", "sub.1.1");
    s.spawn(first);
    up.reached();
    let again = s
        .dibs(["--label", "attempted", &script])
        .env("DIBS_ATTEMPT", "sub.1.1")
        .run();
    assert_eq!(again.code, 75, "{}", again.all());
    assert_eq!(
        again.stderr.lines_with("runs already as job"),
        1,
        "{}",
        again.all()
    );
    hold.open();
    assert_eq!(ran(&s), 1);
}

#[test]
fn an_attempt_id_that_names_a_path_is_refused() {
    let s = Sandbox::new();
    let out = s
        .dibs(["--label", "attempted", "true"])
        .env("DIBS_ATTEMPT", "../escape")
        .run();
    assert_ne!(out.code, 0, "{}", out.all());
    assert!(!s.path("escape").exists());
}

#[test]
fn a_lookup_runs_nothing_and_closes_an_attempt_it_never_saw() {
    let s = Sandbox::new();
    let call = |attempt: Option<&str>| {
        let call = s
            .dibs(["--label", "attempted", r#"echo ran >> "$DIBS_SCRATCH/ran""#])
            .env("DIBS_ATTEMPT_LOOKUP", "1");
        match attempt {
            Some(id) => call.env("DIBS_ATTEMPT", id),
            None => call.env_remove("DIBS_ATTEMPT"),
        }
        .run()
    };
    let unnamed = call(None);
    assert_ne!(unnamed.code, 0, "{}", unnamed.all());
    let looked = call(Some("sub.2.1"));
    assert_eq!(looked.code, 1, "{}", looked.all());
    assert_eq!(
        looked.stderr.lines_with("never admitted"),
        1,
        "{}",
        looked.all()
    );
    let late = s
        .dibs(["--label", "attempted", r#"echo ran >> "$DIBS_SCRATCH/ran""#])
        .env("DIBS_ATTEMPT", "sub.2.1")
        .run();
    assert_ne!(late.code, 0, "{}", late.all());
    assert_eq!(ran(&s), 0);
}

#[test]
fn a_lookup_that_is_not_a_run_does_nothing() {
    let s = Sandbox::new();
    let gc = s
        .dibs(["--gc"])
        .env("DIBS_ATTEMPT_LOOKUP", "1")
        .env("DIBS_ATTEMPT", "sub.3.1")
        .run();
    assert_ne!(gc.code, 0, "{}", gc.all());
    assert_eq!(gc.stderr.lines_with("so nothing ran"), 1, "{}", gc.all());
}
