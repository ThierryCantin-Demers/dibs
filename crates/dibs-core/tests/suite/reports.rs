use crate::harness::*;
use crate::snapshot::*;

/// A gh that answers from files in `ghd/` and writes down every call. Its webhook forward posts
/// `ghd/event.json` to dibs's listener once it has said it is forwarding, then holds until killed.
fn fake_gh(s: &Sandbox, visibility: &str) {
    let d = s.p("ghd");
    s.write("ghd/visibility", &format!("{visibility}\n"));
    s.write("ghd/issues.json", "[]\n");
    s.write_exec(
        "bin/gh",
        &format!(
            r#"#!/bin/bash
d={d}
printf '%s\n' "$*" >> "$d/gh.log"
case "$1 $2" in
    "repo view") cat "$d/visibility" ;;
    "issue create") echo "https://github.com/o/r/issues/7" ;;
    "issue list") cat "$d/issues.json" ;;
    "issue comment") echo "https://github.com/o/r/issues/$3#issuecomment-99" ;;
    "issue close") ;;
    "webhook forward")
        while [ $# -gt 0 ]; do [ "$1" = --url ] && url=$2; shift; done
        echo "Forwarding Webhook events from GitHub..." >&2
        if [ -e "$d/stop-once" ]; then rm "$d/stop-once"; echo "error: the websocket closed" >&2; exit 1; fi
        curl -s -X POST -H "X-GitHub-Event: issue_comment" --data @"$d/event.json" "$url" >/dev/null
        mkfifo "$d/hold.$$"; read -r _ < "$d/hold.$$" ;;
esac
"#
        ),
    );
}

fn gh_log(s: &Sandbox) -> String {
    std::fs::read_to_string(s.path("ghd/gh.log")).unwrap_or_default()
}

fn friction_log(s: &Sandbox) -> String {
    s.read("home/.local/state/dibs/friction.jsonl")
}

#[test]
fn a_report_is_filed_only_where_its_person_said_and_never_in_a_public_repo() {
    let s = Sandbox::new();
    fake_gh(&s, "PRIVATE");
    s.dibs(["--friction", "the flag is missing"]).run();
    assert_eq!(gh_log(&s), "", "without DIBS_REPORTS a report stays on this machine");

    let out = s.dibs(["--friction", "--device is ignored"]).env("DIBS_REPORTS", "o/r").run();
    assert!(out.stdout.contains("filed as o/r#7"), "{}", out.all());
    assert_eq!(gh_log(&s).lines_with("issue create -R o/r --title --device is ignored"), 1, "{}", gh_log(&s));
    assert_eq!(gh_log(&s).lines_with("--label dibs-friction"), 1);
    assert_eq!(friction_log(&s).lines_with(r#""issue":7"#), 1, "and remembers where, for the answer");

    s.write("ghd/visibility", "PUBLIC\n");
    let out = s.dibs(["--friction", "another"]).env("DIBS_REPORTS", "o/r").run();
    assert!(out.stdout.contains("not filed in o/r: o/r is public"), "{}", out.all());
    assert_eq!(gh_log(&s).lines_with("issue create"), 1, "nothing was created there");
    assert_eq!(friction_log(&s).lines_with("another"), 1, "and it is still recorded here");
}

#[test]
fn an_answer_reaches_the_session_that_reported_it_once() {
    let s = Sandbox::new();
    fake_gh(&s, "PRIVATE");
    s.write("home/.local/state/dibs/friction.jsonl", "{\"t\":1,\"text\":\"x\",\"by\":\"me\",\"dibs\":\"a\",\"issue\":7}\n");
    s.write(
        "ghd/issues.json",
        r#"[{"number":7,"state":"OPEN","comments":[{"url":"https://github.com/o/r/issues/7#issuecomment-3","author":{"login":"maintainer"},"body":"fixed in abc, run dibs --update\nmore"}]}]"#,
    );
    let call = |who: &str| s.dibs(["--label", "l", "true"]).no_session().env("DIBS_AGENT", who).env("DIBS_REPORTS", "o/r").run();
    let answered = "your report #7 was answered by maintainer: fixed in abc, run dibs --update";
    assert_eq!(call("other").stderr.lines_with(answered), 0, "not to a session that did not report it");
    std::fs::remove_file(s.path("home/.local/state/dibs/reports-asked")).unwrap();
    assert_eq!(call("me").stderr.lines_with(answered), 1);
    std::fs::remove_file(s.path("home/.local/state/dibs/reports-asked")).unwrap();
    assert_eq!(call("me").stderr.lines_with(answered), 0, "once");
}

#[test]
fn a_wait_reads_what_landed_before_it_listened_then_wakes_on_the_next_one() {
    let s = Sandbox::new();
    fake_gh(&s, "PRIVATE");
    s.write(
        "ghd/issues.json",
        r#"[{"number":5,"title":"the flag is missing","url":"https://github.com/o/r/issues/5","state":"OPEN","author":{"login":"someone"},"comments":[]}]"#,
    );
    let out = s.dibs(["--friction", "--wait"]).env("DIBS_REPORTS", "o/r").run();
    assert_eq!((out.code, out.stdout.lines_with("report #5 from someone: the flag is missing")), (0, 1), "{}", out.all());
    assert_eq!(gh_log(&s).lines_with("webhook forward"), 0, "a backlog needs no listener");

    s.write(
        "ghd/event.json",
        r#"{"action":"created","issue":{"number":5,"labels":[{"name":"dibs-friction"}]},"comment":{"html_url":"https://github.com/o/r/issues/5#issuecomment-1","user":{"login":"someone"},"body":"works now"}}"#,
    );
    let out = s.dibs(["--friction", "--wait"]).env("DIBS_REPORTS", "o/r").run();
    assert_eq!((out.code, out.stdout.lines_with("comment on #5 from someone: works now")), (0, 1), "{}", out.all());
}

#[test]
fn an_answer_posted_from_here_is_its_own() {
    let s = Sandbox::new();
    fake_gh(&s, "PRIVATE");
    let out = s.dibs(["--friction", "--reply", "5", "fixed in abc", "--close"]).env("DIBS_REPORTS", "o/r").run();
    assert_eq!((out.code, out.stdout.lines_with("issues/5#issuecomment-99")), (0, 1), "{}", out.all());
    assert_eq!(gh_log(&s).lines_with("issue comment 5 -R o/r --body fixed in abc"), 1, "{}", gh_log(&s));
    assert_eq!(gh_log(&s).lines_with("<!-- dibs --friction --reply -->"), 1, "marked, so a wait never wakes on it");
    assert_eq!(gh_log(&s).lines_with("issue close 5 -R o/r"), 1);
}

#[test]
fn a_forwarder_that_stops_is_started_again_rather_than_listened_past() {
    let s = Sandbox::new();
    fake_gh(&s, "PRIVATE");
    s.write("home/.local/state/dibs/reports-woken", "#5\n");
    s.write("ghd/stop-once", "");
    s.write(
        "ghd/event.json",
        r#"{"action":"created","issue":{"number":5,"labels":[{"name":"dibs-friction"}]},"comment":{"html_url":"https://github.com/o/r/issues/5#issuecomment-2","user":{"login":"someone"},"body":"again"}}"#,
    );
    let out = s.dibs(["--friction", "--wait"]).env("DIBS_REPORTS", "o/r").run();
    assert_eq!((out.code, out.stdout.lines_with("comment on #5 from someone: again")), (0, 1), "{}", out.all());
    assert_eq!(out.stderr.lines_with("started again. It said: error: the websocket closed"), 1, "{}", out.all());
}

#[test]
fn what_reporting_prints() {
    let s = Sandbox::new();
    fake_gh(&s, "PRIVATE");
    let n = Normal::of(&s).clocked();
    let mut t = Transcript::default();
    let reported = |args: &[&str], reports: Option<&str>| {
        let mut call = s.dibs(args);
        if let Some(r) = reports {
            call = call.env("DIBS_REPORTS", r);
        }
        call.run()
    };
    t.section("dibs --friction 'the flag is missing'", &n.output(&reported(&["--friction", "the flag is missing"], None)));
    t.section("DIBS_REPORTS=o/r dibs --friction '--device is ignored'", &n.output(&reported(&["--friction", "--device is ignored"], Some("o/r"))));
    s.write("ghd/visibility", "PUBLIC\n");
    t.section("DIBS_REPORTS=o/r dibs --friction another  (with o/r public)", &n.output(&reported(&["--friction", "another"], Some("o/r"))));
    s.write("ghd/visibility", "PRIVATE\n");
    s.write(
        "ghd/issues.json",
        r#"[{"number":7,"title":"--device is ignored","url":"https://github.com/o/r/issues/7","state":"OPEN","author":{"login":"someone"},"comments":[{"url":"https://github.com/o/r/issues/7#issuecomment-3","author":{"login":"a-maintainer"},"body":"fixed in abc, run dibs --update\nmore"}]}]"#,
    );
    t.section("DIBS_REPORTS=o/r dibs --label l true  (once the report is answered)", &n.output(&reported(&["--label", "l", "true"], Some("o/r"))));
    t.section("DIBS_REPORTS=o/r dibs --friction --wait  (with a report waiting)", &n.output(&reported(&["--friction", "--wait"], Some("o/r"))));
    s.write(
        "ghd/event.json",
        r#"{"action":"created","issue":{"number":7,"labels":[{"name":"dibs-friction"}]},"comment":{"html_url":"https://github.com/o/r/issues/7#issuecomment-4","user":{"login":"someone"},"body":"works now"}}"#,
    );
    t.section("DIBS_REPORTS=o/r dibs --friction --wait  (until a comment lands)", &n.output(&reported(&["--friction", "--wait"], Some("o/r"))));
    t.section("DIBS_REPORTS=o/r dibs --friction --reply 7 'fixed in abc' --close", &n.output(&reported(&["--friction", "--reply", "7", "fixed in abc", "--close"], Some("o/r"))));
    t.section("dibs --friction --wait  (with no DIBS_REPORTS)", &n.output(&reported(&["--friction", "--wait"], None)));
    snapshot("reports", t.text());
}
