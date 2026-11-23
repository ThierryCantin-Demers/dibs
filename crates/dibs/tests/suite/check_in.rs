use crate::harness::*;

fn job_named(out: &Output) -> String {
    out.stderr
        .split_whitespace()
        .skip_while(|word| *word != "job")
        .nth(1)
        .unwrap_or_else(|| panic!("no job named: {}", out.all()))
        .to_string()
}

#[test]
fn a_job_past_its_check_in_is_let_go_and_followed_to_its_end() {
    let mut s = Sandbox::new();
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let checked = s
        .remote(s.dibs([
            "--check-in",
            "1s",
            "--label",
            "long",
            &format!(
                "echo started; {}; {}; echo finished; exit 3",
                up.signal(),
                hold.hold()
            ),
        ]))
        .run();
    assert_eq!(checked.code, 80, "{}", checked.all());
    assert_eq!(
        checked.stderr.lines_with("This call lets go of it"),
        1,
        "{}",
        checked.all()
    );
    assert_eq!(
        checked.stderr.lines_with("| started"),
        1,
        "{}",
        checked.all()
    );
    let job = job_named(&checked);
    assert_eq!(
        checked.stderr.lines_with(&format!("--follow {job}")),
        1,
        "{}",
        checked.all()
    );
    up.reached();
    s.held(1);

    let again = s.remote(s.dibs(["--follow", &job])).run();
    assert_eq!(again.code, 80, "{}", again.all());
    assert_eq!(
        again.stderr.lines_with("This call lets go of it"),
        1,
        "{}",
        again.all()
    );
    s.held(1);

    let follow = s.spawn(s.remote(s.dibs(["follow", &job])));
    hold.open();
    let ended = s.wait(follow);
    assert_eq!(ended, 3, "the follower ends with the job's own exit");
    s.held(0);
    let log = s.log();
    assert!(
        log.lines()
            .any(|l| l.contains("handed-over") && l.contains(&job)),
        "{log}"
    );
}

#[test]
fn the_follower_passes_on_the_trailer_of_a_job_already_ended() {
    let s = Sandbox::new();
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let checked = s
        .remote(s.dibs([
            "--check-in",
            "1s",
            &format!("{}; {}; echo finished", up.signal(), hold.hold()),
        ]))
        .run();
    assert_eq!(checked.code, 80, "{}", checked.all());
    let job = job_named(&checked);
    up.reached();
    hold.open();
    s.held(0);
    let follow = s.remote(s.dibs(["--follow", &job])).run();
    assert_eq!(follow.code, 0, "{}", follow.all());
    assert_eq!(follow.stdout.lines_with("finished"), 1, "{}", follow.all());
}

#[test]
fn a_job_that_ends_before_its_check_in_keeps_its_call() {
    let s = Sandbox::new();
    let quick = s.remote(s.dibs(["--check-in", "1h", "echo quick"])).run();
    assert_eq!(quick.code, 0, "{}", quick.all());
    let job = quick
        .stderr
        .lines()
        .find_map(|line| line.strip_prefix("job "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no trailer: {}", quick.all()))
        .to_string();
    let follow = s.remote(s.dibs(["--follow", &job])).run();
    assert_eq!(follow.code, 2, "{}", follow.all());
    assert_eq!(
        follow.stderr.lines_with("never let go of by its call"),
        1,
        "{}",
        follow.all()
    );
}

#[test]
fn a_local_call_lets_go_of_its_job_too() {
    let mut s = Sandbox::new();
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let checked = s
        .dibs([
            "--check-in",
            "1s",
            &format!("{}; {}; exit 4", up.signal(), hold.hold()),
        ])
        .run();
    assert_eq!(checked.code, 80, "{}", checked.all());
    let job = job_named(&checked);
    up.reached();
    s.held(1);
    let follow = s.spawn(s.dibs(["--follow", &job]));
    hold.open();
    assert_eq!(s.wait(follow), 4);
}

#[test]
fn a_job_killed_after_its_check_in_is_followed_to_its_kill() {
    let s = Sandbox::new();
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let checked = s
        .remote(s.dibs([
            "--check-in",
            "1s",
            &format!("{}; {}", up.signal(), hold.hold()),
        ]))
        .run();
    assert_eq!(checked.code, 80, "{}", checked.all());
    let job = job_named(&checked);
    let pid = checked
        .stderr
        .lines()
        .find_map(|line| line.split("--kill ").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no kill named: {}", checked.all()))
        .to_string();
    up.reached();
    let kill = s.remote(s.dibs(["--kill", &pid])).run();
    assert_eq!(kill.code, 0, "{}", kill.all());
    s.held(0);
    let follow = s.remote(s.dibs(["--follow", &job])).run();
    assert_eq!(follow.code, 143, "{}", follow.all());
}
