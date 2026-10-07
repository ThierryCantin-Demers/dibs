use super::{
    base::{State, StepStderr},
    parse::{Step, StepKind, parse, split_words},
    plan::Batch,
};
use crate::call::{BatchStep, Pending, Planned};
use dibs_format::{Label, Mode};
use std::path::Path;

fn names(v: &[usize], steps: &[Step]) -> Vec<String> {
    v.iter().map(|&i| steps[i].name.clone()).collect()
}

#[test]
fn a_list_runs_top_to_bottom_unless_a_step_says_what_it_waits_for() {
    let s =
        parse("dibs --label a 'true'\n\ndibs --label b 'true'\n# a comment\n[c after=] dibs x\n")
            .unwrap();
    assert_eq!(
        s.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
        ["1", "2", "c"]
    );
    assert_eq!(s[0].after, Vec::<String>::new());
    assert_eq!(s[1].after, ["1"]);
    assert!(s[2].after.is_empty());
}

#[test]
fn attributes_name_a_step_its_dependencies_and_whether_a_failure_stops_the_rest() {
    let s = parse("[build] dibs --on a --label b 'cargo build'\n[m1 after=build cont] dibs --bench --on a --device gpu:x --label m 'cargo bench'\n").unwrap();
    assert_eq!(s[1].name, "m1");
    assert_eq!(s[1].after, ["build"]);
    assert!(s[1].cont);
    assert_eq!(
        (
            s[1].on.as_deref(),
            s[1].lock,
            s[1].device.as_deref(),
            s[1].label.as_deref()
        ),
        (Some("a"), StepKind::Bench, Some("gpu:x"), Some("m"))
    );
    assert_eq!(
        s[1].line,
        "dibs --bench --on a --device gpu:x --label m 'cargo bench'"
    );
}

#[test]
fn a_step_continued_over_lines_is_one_step() {
    let s = parse("[m] dibs --bench --label x \\\n    'cargo bench'\n").unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(
        split_words(&s[0].line).unwrap(),
        ["dibs", "--bench", "--label", "x", "cargo bench"]
    );
}

#[test]
fn what_a_step_is_comes_from_its_own_flags() {
    let one = |l: &str| parse(l).unwrap().remove(0);
    assert_eq!(one("dibs --sync -a ./x :~/y").lock, StepKind::Sync);
    assert_eq!(one("dibs --peek 'ls'").lock, StepKind::Peek);
    assert_eq!(one("dibs run --bench 'x'").lock, StepKind::Bench);
    let r = one("dibs bench cubek@local reduce --device gpu:0");
    assert_eq!(
        (r.lock, r.label.as_deref()),
        (StepKind::Recipe, Some("bench cubek@local reduce"))
    );
    let r = one("dibs bench cubek@a,b gemv --on m --backend cpu --device gpu:0 -- --on x");
    assert_eq!(
        (r.on.as_deref(), r.device.as_deref()),
        (Some("m"), Some("gpu:0")),
        "read after the recipe too, up to its command"
    );
    let r = one("dibs --wait $W --max \"$M\" --label x 'true'");
    assert_eq!(
        (r.lock, r.label.as_deref()),
        (StepKind::Shared, Some("x")),
        "a number bash has yet to expand is read as unknown, not refused"
    );
    assert_eq!(
        one("dibs bench app@local r --reps $N").lock,
        StepKind::Recipe
    );
}

#[test]
fn anything_that_is_not_one_dibs_call_is_refused() {
    for (line, why) in [
        ("cargo build", "not a dibs command"),
        ("dibs 'true' && rm -rf x", "more than one dibs call"),
        ("dibs 'true' | tail", "more than one dibs call"),
        ("dibs $(whoami)", "runs a command here"),
        ("dibs --detach 'x'", "--detach is gone"),
        ("dibs --watch", "cannot --watch"),
        ("dibs batch steps.txt", "cannot be a batch"),
        ("dibs 'unclosed", "not closed"),
        ("[a b=c] dibs x", "unknown attribute"),
    ] {
        let e = parse(line).unwrap_err().to_string();
        assert!(e.contains(why), "{line}: {e}");
    }
    assert!(
        parse("dibs 'a && b; c | d'").is_ok(),
        "operators inside quotes belong to the remote command"
    );
}

#[test]
fn names_and_dependencies_must_make_sense() {
    assert!(
        parse("[a] dibs x\n[a] dibs y")
            .unwrap_err()
            .to_string()
            .contains("second step is named")
    );
    assert!(
        parse("[a after=zz] dibs x")
            .unwrap_err()
            .to_string()
            .contains("no step has that name")
    );
    assert!(
        parse("[a after=b] dibs x\n[b after=a] dibs y")
            .unwrap_err()
            .to_string()
            .contains("waits on itself")
    );
    assert!(
        parse("# only a comment\n")
            .unwrap_err()
            .to_string()
            .contains("no steps")
    );
}

fn st(n: &str, after: &[&str]) -> Step {
    Step {
        name: n.into(),
        line: String::new(),
        after: after.iter().map(|s| s.to_string()).collect(),
        cont: false,
        on: None,
        lock: StepKind::Shared,
        label: None,
        device: None,
        recipe: None,
    }
}

#[test]
fn independent_steps_overlap_only_on_different_machines() {
    let steps = [
        st("build", &[]),
        st("m1", &["build"]),
        st("m2", &["build"]),
        st("m3", &["build"]),
    ];
    let machines: Vec<String> = ["x", "x", "y", "x"].iter().map(|s| s.to_string()).collect();
    let batch = Batch {
        steps: &steps,
        machines: &machines,
    };
    let mut states = vec![State::Waiting; 4];
    assert_eq!(names(&batch.ready(&states, false), &steps), ["build"]);
    states[0] = State::Running;
    assert!(batch.ready(&states, false).is_empty());
    states[0] = State::Done {
        exit: 0,
        seconds: 1,
    };
    assert_eq!(
        names(&batch.ready(&states, false), &steps),
        ["m1", "m2"],
        "m3 waits for x to be free"
    );
    states[1] = State::Running;
    states[2] = State::Running;
    assert!(batch.ready(&states, false).is_empty());
    states[1] = State::Done {
        exit: 0,
        seconds: 1,
    };
    assert_eq!(names(&batch.ready(&states, false), &steps), ["m3"]);
    assert!(
        batch.ready(&states, true).is_empty(),
        "a stopped batch starts nothing"
    );
}

#[test]
fn a_failed_step_still_releases_what_waits_on_it_when_the_batch_goes_on() {
    let steps = [st("a", &[]), st("b", &["a"])];
    let machines = vec!["x".to_string(), "x".to_string()];
    let batch = Batch {
        steps: &steps,
        machines: &machines,
    };
    let states = vec![
        State::Done {
            exit: 1,
            seconds: 0,
        },
        State::Waiting,
    ];
    assert_eq!(names(&batch.ready(&states, false), &steps), ["b"]);
}

fn call(name: &str, mode: Mode) -> Pending {
    Pending {
        name: name.into(),
        mode: Planned::Job(mode),
        label: Label::new(format!("{name}/x")),
        here: true,
    }
}

#[test]
fn a_step_carries_what_is_still_to_come_under_the_keys_its_history_is_filed_by() {
    let env = BatchStep::new(
        "b1",
        "build",
        1,
        3,
        &[
            call("bench", Mode::Bench),
            Pending {
                here: false,
                ..call("home", Mode::Rsh)
            },
        ],
    );
    assert_eq!(env.batch, "b1");
    assert_eq!(env.step, "build");
    assert_eq!(
        env.plan,
        "1\t3\nbench\tbench\tbench_x\t1\nhome\trsh\thome_x\t0\n"
    );
}

#[test]
fn a_recipe_alone_is_its_own_batch_and_inside_one_goes_ahead_of_the_rest() {
    let calls = [
        call("send", Mode::Rsh),
        call("build", Mode::Shared),
        call("bench", Mode::Bench),
    ];
    let alone = BatchStep::for_recipe_job_in(None, "own", &calls, 1).unwrap();
    assert_eq!(alone.batch, "own");
    assert_eq!(alone.plan, "2\t3\nbench\tbench\tbench_x\t1\n");
    assert!(
        BatchStep::for_recipe_job_in(None, "own", &calls[..1], 0).is_none(),
        "one job is not a batch"
    );
    let outer = Some(BatchStep {
        batch: "b9".to_string(),
        step: "arm-a".to_string(),
        plan: "2\t4\narm-b\trecipe\t\t1\n".to_string(),
    });
    let inside = BatchStep::for_recipe_job_in(outer, "own", &calls, 1).unwrap();
    assert_eq!(inside.batch, "b9");
    assert_eq!(inside.step, "arm-a: build");
    assert_eq!(
        inside.plan,
        "2\t4\nbench\tbench\tbench_x\t1\narm-b\trecipe\t\t1\n"
    );
}

#[test]
fn a_step_ended_by_a_signal_reads_as_killed_rather_than_as_an_exit_code() {
    let steps = parse("[a] dibs run true\n[b] dibs run true\n").unwrap();
    let machines = vec!["m".to_string(), "m".to_string()];
    let states = [
        State::Done {
            exit: -1,
            seconds: 6,
        },
        State::Done {
            exit: 3,
            seconds: 1,
        },
    ];
    let batch = Batch {
        steps: &steps,
        machines: &machines,
    };
    let out = batch.summary("1", &states, Path::new("/nonexistent"), 7, None);
    assert!(
        out.lines()
            .any(|l| l.starts_with("a ") && l.contains(" killed")),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.starts_with("b ") && l.contains(" 3 ")),
        "{out}"
    );
}

#[test]
fn only_dibs_saying_so_makes_exit_76_a_cancellation() {
    assert!(
        StepStderr("dibs: batch 1 was cancelled with dibs --kill, so this step does not run.\n")
            .cancelled()
    );
    assert!(
        StepStderr("job 20260917-9  bench  x  queued 0s  ran 4s  exit 76  by=dibs\n").cancelled()
    );
    assert!(
        !StepStderr("job 20260917-9  shared  x  queued 0s  ran 1s  exit 76  by=command\n")
            .cancelled()
    );
}

#[test]
fn the_job_ids_come_from_the_trailers() {
    let err = "dibs: step 1/2\njob 20260916-1  shared  a:setup  queued 0s  ran 1s  exit 0  by=command\n  log m:/x\njob 20260916-2  bench  a  queued 3s  ran 9s  exit 0  by=command  built=nothing\njob 20260916-3  shared  a  queued 0s  ran 0s  exit 69  by=dibs\n";
    assert_eq!(
        StepStderr(err).jobs(),
        [
            "20260916-1",
            "20260916-2 built=nothing",
            "20260916-3 by=dibs"
        ]
    );
}
