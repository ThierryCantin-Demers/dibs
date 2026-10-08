use super::{base::StepPlan, refs::Side, schedule::Job, sweep::SweepPoints, trees::gitdb_args};
use crate::{
    call::Pending,
    cli::{Invocation, RecipeCall},
    recipe::{self, Isolation, Lock, Recipe, Resolved, Step, Verb},
};
use dibs_format::wire;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

fn step(lock: Lock, run: &str) -> Step {
    Step {
        lock,
        run: run.into(),
        env: BTreeMap::new(),
    }
}

// --sync warns that kept mtimes may make a build compile nothing, which is about a source
// tree and never a git database, so dibs's own send must not set it off.
#[test]
fn a_git_database_is_sent_without_its_mtimes() {
    let args = gitdb_args(Path::new("/x"), "db");
    assert!(args.iter().any(|a| a == "--no-times"), "{args:?}");
}

fn swept(words: &[&str]) -> RecipeCall {
    let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
    match Invocation::parse(&words) {
        Ok(Invocation::Recipe(call)) => call,
        other => panic!("not a recipe call: {other:?}"),
    }
}

#[test]
fn a_sweep_is_every_combination_over_the_values_already_given() {
    let args = swept(&[
        "bench",
        "app@local",
        "r",
        "--backend",
        "cuda",
        "--sweep",
        "size=64,128",
        "--sweep",
        "layout=rc,cr",
    ]);
    let points = SweepPoints::of(&args).points;
    let shape: Vec<String> = points
        .iter()
        .map(|p| format!("{} {} {}", p["backend"], p["size"], p["layout"]))
        .collect();
    assert_eq!(
        shape,
        ["cuda 64 rc", "cuda 64 cr", "cuda 128 rc", "cuda 128 cr"]
    );
}

#[test]
fn a_value_with_a_comma_in_it_is_one_value() {
    let args = swept(&[
        "bench",
        "app@local",
        "r",
        "--problems",
        "topk1,topk2",
        "--sweep",
        "samples=10,30",
    ]);
    let points = SweepPoints::of(&args).points;
    assert_eq!(points.len(), 2, "only the sweep multiplies the runs");
    assert_eq!(points[0]["problems"], "topk1,topk2");
}

#[test]
fn a_swept_run_is_a_batch_of_ordinary_calls() {
    let args = swept(&[
        "bench",
        "app@local",
        "r",
        "--device",
        "gpu0",
        "--sweep",
        "samples=10,30",
        "--reps",
        "2",
        "--anyway",
        "--new-series",
    ]);
    let text = SweepPoints::of(&args).text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "one call per value, each repeating its own measurement"
    );
    assert_eq!(
        lines[0],
        "[samples-10] dibs bench app@local r --reps 2 --anyway --new-series --device gpu0 --samples 10"
    );
    assert_eq!(
        lines[1],
        "[samples-30] dibs bench app@local r --reps 2 --anyway --new-series --device gpu0 --samples 30"
    );
}

#[test]
fn several_words_after_the_separator_stay_several_words() {
    let parsed = |words: &[&str]| swept(words).command;
    assert_eq!(
        parsed(&[
            "with",
            "app@local",
            "srv",
            "--",
            "bash",
            "-c",
            "exec client ws://$DIBS_SERVICE_WS"
        ])
        .as_deref(),
        Some("bash -c 'exec client ws://$DIBS_SERVICE_WS'")
    );
    assert_eq!(
        parsed(&[
            "shell",
            "app@local",
            "--reason",
            "r",
            "--",
            "echo a; echo $B"
        ])
        .as_deref(),
        Some("echo a; echo $B"),
        "one word is a shell string"
    );
}

#[test]
fn a_swept_shell_carries_its_reason_its_lock_and_its_command_quoted() {
    let args = swept(&[
        "shell",
        "app@main..local",
        "--reason",
        "why not",
        "--bench",
        "--max",
        "60",
        "--reps",
        "2",
        "--sweep",
        "n=1,2",
        "--",
        "echo a; echo b",
    ]);
    let text = SweepPoints::of(&args).text();
    assert_eq!(
        text.lines().next().unwrap(),
        "[n-1] dibs shell app@main..local --bench --max 60 --reps 2 --reason 'why not' --n 1 -- 'echo a; echo b'",
        "a batch line is one dibs call, so anything the shell would read has to be quoted"
    );
}

#[test]
fn a_build_claims_its_target_and_the_measurement_after_it_checks_the_claim() {
    let rec = resolved(vec![
        step(Lock::Shared, "cargo build --release"),
        step(Lock::Exclusive, "cargo bench"),
    ])
    .rec;
    let build = StepPlan::of(&rec, 0, "t", "t", false, None);
    assert_eq!(build.command, "cargo build --release");
    assert_eq!(
        build.around,
        wire::Step {
            claim: true,
            record: Some("t".into()),
            ..wire::Step::default()
        }
    );
    let measured = wire::Step {
        record: Some("t".into()),
        state: true,
        ..wire::Step::default()
    };
    assert_eq!(
        StepPlan::of(&rec, 1, "t", "t", false, None).around,
        wire::Step {
            check: true,
            ..measured.clone()
        }
    );
    assert_eq!(StepPlan::of(&rec, 1, "t", "t", true, None).around, measured);
}

// Nothing was built into the target, so whatever claimed it last says nothing about this run.
#[test]
fn a_measurement_after_no_build_is_not_checked() {
    let rec = resolved(vec![
        step(Lock::Shared, "make data"),
        step(Lock::Exclusive, "./bench.sh"),
    ])
    .rec;
    assert_eq!(
        StepPlan::of(&rec, 1, "t", "t", false, None).around,
        wire::Step {
            state: true,
            ..wire::Step::default()
        }
    );
}

#[test]
fn only_a_shared_build_against_pins_checks_they_took() {
    let rec = resolved(vec![
        step(Lock::Shared, "cargo build --release"),
        step(Lock::Exclusive, "cargo bench"),
    ])
    .rec;
    let names: BTreeSet<String> = ["serde".to_string()].into();
    assert_eq!(
        StepPlan::of(&rec, 0, "t", "t", false, Some(&names))
            .around
            .pinned,
        ["serde"]
    );
    assert!(
        StepPlan::of(&rec, 1, "t", "t", false, Some(&names))
            .around
            .pinned
            .is_empty()
    );
}

#[test]
fn a_recipe_s_jobs_are_what_it_will_send_and_run() {
    let names = |jobs: Vec<Pending>| {
        jobs.into_iter()
            .map(|j| format!("{} {}", j.mode, j.label))
            .collect::<Vec<_>>()
    };
    let build_then_bench = resolved(vec![
        step(Lock::Shared, "cargo build"),
        step(Lock::Exclusive, "cargo bench"),
    ]);
    assert_eq!(
        names(Job::pending(
            &build_then_bench,
            &[Side::Local],
            &[true],
            1,
            &[]
        )),
        [
            "rsh app/bench/r:send",
            "shared app/bench/r",
            "bench app/bench/r"
        ]
    );
    assert_eq!(
        names(Job::pending(
            &build_then_bench,
            &[Side::Ref("main".into())],
            &[false],
            1,
            &[]
        )),
        ["shared app/bench/r", "bench app/bench/r"],
        "the setup rides with the build"
    );
    let bench_only = resolved(vec![step(Lock::Exclusive, "cargo bench")]);
    assert_eq!(
        names(Job::pending(
            &bench_only,
            &[Side::Ref("main".into())],
            &[false],
            1,
            &[]
        )),
        ["shared app/bench/r:setup", "bench app/bench/r"],
        "never inside the hold"
    );
}

#[test]
fn a_ref_is_one_tree_a_range_is_a_tip_against_its_base_and_a_list_is_arms_in_turn() {
    let r = |s: &str| Side::Ref(s.into());
    assert_eq!(Side::list(None).unwrap(), [r("HEAD")]);
    assert_eq!(Side::list(Some("local")).unwrap(), [Side::Local]);
    assert_eq!(
        Side::list(Some("main..local")).unwrap(),
        [Side::Base("main".into(), "local".into()), Side::Local]
    );
    assert_eq!(
        Side::list(Some("main..perf/x")).unwrap(),
        [
            Side::Base("main".into(), "perf/x".into()),
            Side::Pinned("perf/x".into())
        ]
    );
    assert_eq!(
        Side::list(Some("a1,b2,local")).unwrap(),
        [r("a1"), r("b2"), Side::Local]
    );
    for bad in [
        "main...local",
        "..local",
        "main..",
        "a..b..c",
        "a..b,c",
        "a,,b",
        "a,a",
        "local,local",
    ] {
        assert!(Side::list(Some(bad)).is_err(), "{bad}");
    }
}

#[test]
fn every_arm_is_built_before_any_is_measured_and_the_order_turns_each_rep() {
    let build_then_bench = [
        step(Lock::Shared, "cargo build"),
        step(Lock::Exclusive, "cargo bench"),
    ];
    let s = |arm, step, rep, setup| Job::Step {
        arm,
        step,
        rep,
        setup,
    };
    assert_eq!(
        Job::schedule(&[false, true], &build_then_bench, 2),
        [
            s(0, 0, None, true),
            Job::Send(1),
            s(1, 0, None, false),
            s(0, 1, Some(1), false),
            s(1, 1, Some(1), false),
            s(1, 1, Some(2), false),
            s(0, 1, Some(2), false),
        ]
    );
    let bench_only = [step(Lock::Exclusive, "./bench")];
    assert_eq!(
        Job::schedule(&[false], &bench_only, 2),
        [
            Job::Setup(0),
            s(0, 0, Some(1), false),
            s(0, 0, Some(2), false)
        ],
        "a tree is set up where its first step needs it, never inside the hold"
    );
    let test = [step(Lock::Shared, "cargo test")];
    assert_eq!(
        Job::schedule(&[false], &test, 2),
        [s(0, 0, Some(1), true), s(0, 0, Some(2), false)],
        "nothing exclusive, so all of it repeats"
    );
}

#[test]
fn a_comparison_s_jobs_say_which_arm_and_rep_they_are() {
    let r = resolved(vec![
        step(Lock::Shared, "cargo build"),
        step(Lock::Exclusive, "cargo bench"),
    ]);
    let names: Vec<String> = Job::pending(
        &r,
        &Side::list(Some("main..local")).unwrap(),
        &[true, true],
        2,
        &[],
    )
    .into_iter()
    .map(|j| j.name)
    .collect();
    assert_eq!(
        names,
        [
            "app/bench/r:send (base)",
            "app/bench/r (base)",
            "app/bench/r:send (local)",
            "app/bench/r (local)",
            "app/bench/r (base r1)",
            "app/bench/r (local r1)",
            "app/bench/r (local r2)",
            "app/bench/r (base r2)",
        ]
    );
}

// Both steps of one run see one store, and the next run another.
#[test]
fn a_fresh_variable_has_one_value_per_run_in_every_step() {
    let mut rec = resolved(vec![
        step(Lock::Shared, "make"),
        step(Lock::Exclusive, "./bench"),
    ])
    .rec;
    rec.fresh = vec!["CUBECL_ENVIRONMENT".into()];
    for i in 0..2 {
        assert!(
            StepPlan::of(&rec, i, "t1", "t1", false, None)
                .command
                .starts_with("export CUBECL_ENVIRONMENT=dibs-t1; ")
        );
    }
    assert!(
        StepPlan::of(&rec, 1, "t2", "t2", false, None)
            .command
            .starts_with("export CUBECL_ENVIRONMENT=dibs-t2; ")
    );
}

fn resolved(steps: Vec<Step>) -> Resolved {
    let rec = Recipe {
        source: recipe::Source::Local,
        needs: None,
        isolation: Isolation::Machine,
        params: BTreeMap::new(),
        fresh: Vec::new(),
        artifacts: Vec::new(),
        steps,
    };
    let step_labels = rec.step_labels("app/bench/r");
    Resolved {
        dir: PathBuf::from("."),
        repo_name: "app".into(),
        verb: Verb::Bench,
        name: "r".into(),
        rec,
        label: "app/bench/r".into(),
        step_labels,
        shell_reason: None,
        params: BTreeMap::new(),
        tree_fresh: Vec::new(),
    }
}
