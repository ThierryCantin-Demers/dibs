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
    s.until_records("the job ended", || s.holders() == 0);
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
    std::fs::File::open(s.path(&format!("scratch/jobs/{job}/handed")))
        .and_then(|handed| handed.lock_shared())
        .expect("the runner that let go of the job ended");
    let follow = s.remote(s.dibs(["--follow", &job])).run();
    assert_eq!(follow.code, 0, "{}", follow.all());
    assert_eq!(follow.stdout.lines_with("finished"), 1, "{}", follow.all());
    assert_eq!(
        follow
            .stderr
            .lines()
            .filter(|l| l.starts_with(&format!("job {job} ")) && l.contains("exit 0"))
            .count(),
        1,
        "{}",
        follow.all()
    );
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
    s.until_records("the job ended", || s.holders() == 0);
    let follow = s.remote(s.dibs(["--follow", &job])).run();
    assert_eq!(follow.code, 143, "{}", follow.all());
}

#[test]
fn a_batch_step_that_checks_in_pauses_the_batch_until_it_is_followed() {
    let mut s = Sandbox::new();
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let order = s.p("order");
    s.write(
        "b",
        &format!(
            "[long] dibs --check-in 1s --label long '{}; {}; echo long >> {order}'\n\
             [next] dibs --label next 'echo next >> {order}'\n",
            up.signal(),
            hold.hold()
        ),
    );
    let paused = s.dibs(["batch", &s.p("b")]).run();
    assert_eq!(paused.code, 80, "{}", paused.all());
    let id = capture(&paused.stderr, r"batch ([0-9-]+) checked in")
        .unwrap_or_else(|| panic!("no check-in: {}", paused.all()));
    assert_eq!(
        paused.stderr.lines_with("This call lets go of it"),
        1,
        "the step's own check-in is shown: {}",
        paused.all()
    );
    assert_eq!(
        paused
            .stderr
            .lines_with(&format!("dibs batch --follow {id}")),
        1,
        "{}",
        paused.all()
    );
    assert_eq!(
        paused.stderr.lines_matching(r"--follow [0-9]{14}-"),
        0,
        "the job is followed through its batch, never alone: {}",
        paused.all()
    );
    up.reached();
    s.held(1);
    assert!(
        !s.read("order").contains("next"),
        "nothing starts meanwhile"
    );

    let follow = s.spawn(s.dibs(["batch", "--follow", &id]));
    hold.open();
    assert_eq!(s.wait(follow), 0);
    assert_eq!(s.read("order"), "long\nnext\n");
    let again = s.dibs(["batch", "--follow", &id]).run();
    assert_eq!(again.code, 0, "an ended batch is followed to its summary");
    assert_eq!(
        again.stdout.lines_matching(r"^long  .* [0-9]{14}-[0-9]+$"),
        1,
        "{}",
        again.all()
    );
}

#[test]
fn a_shell_that_checks_in_is_recorded_by_its_follower() {
    let mut s = Sandbox::new();
    let app = crate::recipes::app(&s);
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    let checked = s
        .dibs([
            "shell",
            &format!("{app}@local"),
            "--reason",
            "test",
            "--",
            &format!("{}; {}; exit 5", up.signal(), hold.hold()),
        ])
        .env("DIBS_CHECK_IN", "1s")
        .run();
    assert_eq!(checked.code, 80, "{}", checked.all());
    assert_eq!(
        checked
            .stderr
            .lines_with("by the dibs --follow that sees it end"),
        1,
        "{}",
        checked.all()
    );
    let job = capture(&checked.stderr, r"--follow ([0-9]+-[0-9]+)")
        .unwrap_or_else(|| panic!("no follow: {}", checked.all()));
    let file = s.path("home/.local/state/dibs/runs.jsonl");
    let runs = || std::fs::read_to_string(&file).unwrap_or_default();
    assert!(
        !runs().contains("\"verb\":\"shell\""),
        "nothing is recorded before the job ends"
    );
    up.reached();
    s.held(1);
    let follow = s.spawn(s.dibs(["--follow", &job]));
    hold.open();
    assert_eq!(s.wait(follow), 5, "the follower ends with the job's exit");
    let runs = runs();
    let run = runs
        .lines()
        .find(|l| l.contains("\"verb\":\"shell\""))
        .unwrap_or_else(|| panic!("no run recorded: {runs}"));
    assert!(run.contains("\"status\":5") && run.contains(&job), "{run}");
    let twice = s.dibs(["--follow", &job]).run();
    assert_eq!(twice.code, 5, "{}", twice.all());
    assert_eq!(
        std::fs::read_to_string(&file)
            .unwrap_or_default()
            .lines_with("\"verb\":\"shell\""),
        1,
        "a second follower records nothing more"
    );
}
