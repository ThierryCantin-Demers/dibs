use crate::cli::{
    Call, CliError, Command, Friction, Help, Invocation, KillTarget, Mode, OutTarget, PortName,
    RecipeCall, RecipeVerb, Run, RunLock, Service, ServiceName, Sweep,
};
use dibs_format::{Alias, BatchId, JobId, Label, MachineName};
use std::collections::BTreeMap;

#[test]
fn every_form_reads_as_the_bash_client_read_it() {
    for (words, expected) in calls() {
        assert_eq!(parse(&words), Ok(expected), "dibs {}", words.join(" "));
    }
}

#[test]
fn every_recipe_form_reads_as_the_recipe_layer_read_it() {
    for (words, expected) in recipes() {
        assert_eq!(parse(&words), Ok(expected), "dibs {}", words.join(" "));
    }
}

#[test]
fn a_refusal_says_what_the_bash_client_said() {
    for (words, message, with_help) in refusals() {
        let expected = CliError {
            message: message.to_string(),
            with_help,
        };
        assert_eq!(parse(&words), Err(expected), "dibs {}", words.join(" "));
    }
}

/// The refusal snapshot is what bash dibs printed: every refusal that needs no machine and no
/// inventory is refused here, word for word.
#[test]
fn the_refusals_the_suite_recorded_are_refused_in_the_same_words() {
    let text = include_str!("../../tests/suite/snapshots/refusals.txt");
    let decided_later = ["--friction '   '"];
    let mut checked = 0;
    for section in text.split("\n== ").skip(1) {
        let (title, body) = section.split_once('\n').unwrap();
        let Some(args) = title.strip_prefix("dibs").map(str::trim) else {
            continue;
        };
        let result = Invocation::parse(&split(args));
        if body.starts_with("-> known I1") {
            let flag = split(args).into_iter().last().unwrap();
            let expected = CliError::needs_value(&flag);
            assert_eq!(result, Err(expected), "{title}");
            checked += 1;
            continue;
        }
        if decided_later.contains(&args) {
            assert!(result.is_ok(), "{title}");
            continue;
        }
        let error = result.expect_err(title);
        let said: Vec<&str> = body
            .lines()
            .filter_map(|l| l.strip_prefix("2| ").or(l.strip_prefix("2|")))
            .collect();
        assert_eq!(error.message, said.join("\n"), "{title}");
        assert_eq!(
            error.with_help,
            body.contains("1| <what dibs --help prints>"),
            "{title}"
        );
        checked += 1;
    }
    assert!(checked >= 40, "only {checked} refusals were compared");
}

#[test]
fn the_help_is_what_the_bash_client_printed() {
    let snapshot = |text: &str, title: &str| -> String {
        let section = text.split(&format!("== {title}\n")).nth(1).unwrap();
        section
            .lines()
            .skip(1)
            .take_while(|l| l.starts_with("1|"))
            .map(|l| format!("{}\n", l.strip_prefix("1| ").unwrap_or_default()))
            .collect()
    };
    let help = include_str!("../../tests/suite/snapshots/help.txt");
    assert_eq!(Help::text(), snapshot(help, "dibs --help"));
    let recipes = include_str!("../../tests/suite/snapshots/help-recipes.txt");
    assert_eq!(Help::RECIPES, snapshot(recipes, "dibs build --help"));
}

#[test]
fn a_removed_flag_is_refused_as_gone() {
    for flag in [
        "--detach",
        "--jobs",
        "--job",
        "--cancel",
        "--registry-sync",
        "--any",
        "--shared",
        "--abi",
    ] {
        let error = parse(&[flag, "x"]).unwrap_err();
        assert!(
            error
                .message
                .starts_with(&format!("dibs: {flag} is gone: ")),
            "{}",
            error.message
        );
        assert!(!error.with_help);
    }
}

#[test]
fn several_words_are_each_quoted_and_one_is_sent_as_it_is() {
    let Ok(Invocation::Call(Call {
        mode: Mode::Run(run),
        ..
    })) = parse(&["seq", "1", "3"])
    else {
        panic!("a run");
    };
    assert_eq!(run.command.shell_string(), "seq 1 3 ");
}

fn parse(words: &[&str]) -> Result<Invocation, CliError> {
    Invocation::parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn call(mode: Mode) -> Call {
    Call {
        mode,
        ..Call::default()
    }
}

fn run(lock: RunLock, command: &[&str]) -> Mode {
    Mode::Run(Run {
        lock,
        command: Command(words(command)),
        ..Run::default()
    })
}

fn shared(command: &[&str]) -> Invocation {
    Invocation::Call(call(run(RunLock::Shared, command)))
}

fn on(machine: &str, mode: Mode) -> Invocation {
    Invocation::Call(Call {
        on: Some(MachineName::new(machine)),
        ..call(mode)
    })
}

fn of(mode: Mode) -> Invocation {
    Invocation::Call(call(mode))
}

fn calls() -> Vec<(Vec<&'static str>, Invocation)> {
    let bench = |c: &[&str]| of(run(RunLock::Bench, c));
    let peek = |c: &[&str]| of(Mode::Peek(Command(words(c))));
    let fetch = |job: &str, into: Option<&str>| Mode::Fetch {
        job: JobId::new(job),
        into: into.map(str::to_string),
    };
    let kill = |target, force, anyone| Mode::Kill {
        target,
        force,
        anyone,
    };
    let with = |c: Call| Invocation::Call(c);
    vec![
        (vec!["echo hi"], shared(&["echo hi"])),
        (vec!["run", "echo hi"], shared(&["echo hi"])),
        (vec!["seq", "1", "3"], shared(&["seq", "1", "3"])),
        (vec!["--bench", "x"], bench(&["x"])),
        (vec!["-b", "x"], bench(&["x"])),
        (vec!["run", "--bench", "x"], bench(&["x"])),
        (vec!["--bench", "run", "x"], bench(&["x"])),
        (vec!["run", "status"], shared(&["status"])),
        (vec!["run", "build", "x"], shared(&["build", "x"])),
        (vec!["--bench", "status"], bench(&["status"])),
        (vec!["--peek", "ls"], peek(&["ls"])),
        (vec!["--peek", "status"], peek(&["status"])),
        (vec!["--peek", "run", "x"], peek(&["run", "x"])),
        (vec!["--", "--bench"], shared(&["--bench"])),
        (vec!["run", "--", "-x", "y"], shared(&["-x", "y"])),
        (vec!["ls", "--bench"], shared(&["ls", "--bench"])),
        (
            vec!["--on", "m", "run", "x"],
            on("m", run(RunLock::Shared, &["x"])),
        ),
        (vec!["--status"], of(Mode::Status)),
        (vec!["-s"], of(Mode::Status)),
        (vec!["status"], of(Mode::Status)),
        (vec!["--on", "m", "status"], on("m", Mode::Status)),
        (
            vec!["status", "--json", "-v", "--all"],
            with(Call {
                json: true,
                verbose: true,
                all: true,
                ..call(Mode::Status)
            }),
        ),
        (vec!["--watch"], of(Mode::Watch { every: 5 })),
        (vec!["-w", "10"], of(Mode::Watch { every: 10 })),
        (
            vec!["--watch", "2", "--on", "m"],
            on("m", Mode::Watch { every: 2 }),
        ),
        (
            vec!["--watch", "0", "--json"],
            with(Call {
                json: true,
                ..call(Mode::Watch { every: 1 })
            }),
        ),
        (vec!["--log"], of(Mode::Log { lines: 40 })),
        (vec!["--log", "5"], of(Mode::Log { lines: 5 })),
        (vec!["--log", "--on", "m"], on("m", Mode::Log { lines: 40 })),
        (vec!["--release"], of(Mode::Release)),
        (
            vec!["--gc"],
            of(Mode::Gc {
                days: None,
                dry_run: false,
            }),
        ),
        (
            vec!["gc", "--days", "3", "--dry-run"],
            of(Mode::Gc {
                days: Some(3),
                dry_run: true,
            }),
        ),
        (vec!["--out"], of(Mode::Out(None))),
        (
            vec!["--out", "1234"],
            of(Mode::Out(Some(OutTarget::Pid(1234)))),
        ),
        (
            vec!["out", "20261001-120000-77"],
            of(Mode::Out(Some(OutTarget::Job(JobId::new(
                "20261001-120000-77",
            ))))),
        ),
        (vec!["--out", "--on", "m"], on("m", Mode::Out(None))),
        (vec!["--fetch", "1-2"], of(fetch("1-2", None))),
        (vec!["--fetch", "1-2", "dir"], of(fetch("1-2", Some("dir")))),
        (vec!["fetch", "1-2", "dir"], of(fetch("1-2", Some("dir")))),
        (
            vec!["--fetch", "1-2", "--on", "m"],
            on("m", fetch("1-2", None)),
        ),
        (
            vec!["--kill", "123"],
            of(kill(KillTarget::Pid(123), false, false)),
        ),
        (
            vec!["--kill", "20261001-120000-77", "--anyone", "--force"],
            of(kill(
                KillTarget::Batch(BatchId::new("20261001-120000-77")),
                true,
                true,
            )),
        ),
        (
            vec!["--kill", "123", "extra"],
            of(kill(KillTarget::Pid(123), false, false)),
        ),
        (vec!["--check"], of(Mode::Check { host: None })),
        (
            vec!["--check", "box", "--write"],
            with(Call {
                write: true,
                ..call(Mode::Check {
                    host: Some("box".into()),
                })
            }),
        ),
        (
            vec!["--check", "--write"],
            with(Call {
                write: true,
                ..call(Mode::Check { host: None })
            }),
        ),
        (
            vec!["--on", "m", "--sync", "-a", "./x", ":~/y"],
            on("m", Mode::Sync(words(&["-a", "./x", ":~/y"]))),
        ),
        (
            vec!["--rsh", "host", "rsync", "--server", "."],
            of(Mode::Rsh {
                host: "host".into(),
                command: words(&["rsync", "--server", "."]),
            }),
        ),
        (
            vec!["--rsh", "-l", "me", "host", "rsync"],
            of(Mode::Rsh {
                host: "host".into(),
                command: words(&["rsync"]),
            }),
        ),
        (
            vec!["--machines", "-v"],
            with(Call {
                verbose: true,
                ..call(Mode::Machines)
            }),
        ),
        (vec!["--which"], of(Mode::Which)),
        (vec!["--which", "anything", "--dry-run"], of(Mode::Which)),
        (
            vec!["--pick", "--repo", "app", "--prefer", "m"],
            with(Call {
                repo: Some("app".into()),
                prefer: Some("m".into()),
                ..call(Mode::Pick)
            }),
        ),
        (vec!["--update"], of(Mode::Update)),
        (
            vec!["--forget", "m"],
            of(Mode::Forget(MachineName::new("m"))),
        ),
        (
            vec![
                "--label",
                "l",
                "--wait",
                "5",
                "--max",
                "60",
                "--device",
                "gpu:0",
                "--new-series",
                "--stream",
                "--preflight",
                "x",
            ],
            with(Call {
                label: Some(Label::new("l")),
                wait: Some(5),
                max: Some(60),
                device: Some(Alias::new("gpu:0")),
                new_series: true,
                stream: true,
                preflight: true,
                ..call(run(RunLock::Shared, &["x"]))
            }),
        ),
        (vec!["--label", "-h", "x"], {
            with(Call {
                label: Some(Label::new("-h")),
                ..call(run(RunLock::Shared, &["x"]))
            })
        }),
        (
            vec![
                "--hold",
                "--bench",
                "--with",
                "srv=./serve --port $DIBS_PORT_API",
                "--ready",
                "tcp:api",
                "--with",
                "db=./db",
                "--port",
                "api",
                "--ready-within",
                "60",
                "./client",
            ],
            of(Mode::Run(Run {
                lock: RunLock::Bench,
                hold: true,
                services: vec![
                    Service {
                        name: ServiceName("srv".into()),
                        command: "./serve --port $DIBS_PORT_API".into(),
                        ready: Some("tcp:api".into()),
                    },
                    Service {
                        name: ServiceName("db".into()),
                        command: "./db".into(),
                        ready: None,
                    },
                ],
                ports: vec![PortName("api".into())],
                ready_within: 60,
                command: Command(words(&["./client"])),
            })),
        ),
        (
            vec!["--with", "s=x", "--ready", "tcp:host:8080", "y"],
            of(Mode::Run(Run {
                services: vec![Service {
                    name: ServiceName("s".into()),
                    command: "x".into(),
                    ready: Some("tcp:host:8080".into()),
                }],
                command: Command(words(&["y"])),
                ..Run::default()
            })),
        ),
        (vec!["-h"], Invocation::Help),
        (vec!["--help"], Invocation::Help),
        (vec!["status", "-h"], Invocation::Help),
        (
            vec!["--friction", "a line", "more"],
            Invocation::Friction(Friction::Note {
                text: "a line".into(),
            }),
        ),
        (
            vec!["--bench", "--friction", "x"],
            Invocation::Friction(Friction::Note { text: "x".into() }),
        ),
        (
            vec!["friction", "   "],
            Invocation::Friction(Friction::Note { text: "   ".into() }),
        ),
        (
            vec!["--friction", "--wait"],
            Invocation::Friction(Friction::Wait),
        ),
        (
            vec!["--friction", "--reply", "#5", "fixed in abc", "--close"],
            Invocation::Friction(Friction::Reply {
                issue: 5,
                answer: "fixed in abc".into(),
                close: true,
            }),
        ),
    ]
}

fn recipe(verb: RecipeVerb, repo: &str) -> RecipeCall {
    RecipeCall {
        verb,
        repo: repo.into(),
        reference: None,
        recipe: None,
        root: None,
        dry_run: false,
        reason: None,
        command: None,
        device: None,
        on: None,
        params: BTreeMap::new(),
        sweep: Vec::new(),
        reps: 1,
        artifacts_to: None,
        pins: Vec::new(),
        bench: false,
        max: None,
        anyway: false,
        there: false,
        json: false,
        all: false,
        new_series: false,
        verbose: false,
    }
}

fn recipes() -> Vec<(Vec<&'static str>, Invocation)> {
    let r = Invocation::Recipe;
    vec![
        (
            vec!["build", "app@main", "rec", "--device", "gpu:0"],
            r(RecipeCall {
                reference: Some("main".into()),
                recipe: Some("rec".into()),
                device: Some("gpu:0".into()),
                ..recipe(RecipeVerb::Build, "app")
            }),
        ),
        (
            vec!["--on", "m", "bench", "app", "r"],
            r(RecipeCall {
                on: Some("m".into()),
                recipe: Some("r".into()),
                ..recipe(RecipeVerb::Bench, "app")
            }),
        ),
        (
            vec!["--on", "m", "bench", "app", "r", "--on", "n"],
            r(RecipeCall {
                on: Some("n".into()),
                recipe: Some("r".into()),
                ..recipe(RecipeVerb::Bench, "app")
            }),
        ),
        (
            vec![
                "bench",
                "app@main..local",
                "r",
                "--reps",
                "3",
                "--sweep",
                "n=1,2",
                "--backend",
                "cpu",
                "--size=4",
                "--pin",
                "lib@local",
                "--artifacts",
                "out",
                "--anyway",
                "--new-series",
                "--max",
                "90",
                "--root",
                "/src",
                "--dry-run",
            ],
            r(RecipeCall {
                reference: Some("main..local".into()),
                recipe: Some("r".into()),
                reps: 3,
                sweep: vec![Sweep {
                    name: "n".into(),
                    values: words(&["1", "2"]),
                }],
                params: [("backend", "cpu"), ("size", "4")]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                pins: words(&["lib@local"]),
                artifacts_to: Some("out".into()),
                anyway: true,
                new_series: true,
                max: Some(90),
                root: Some("/src".into()),
                dry_run: true,
                ..recipe(RecipeVerb::Bench, "app")
            }),
        ),
        (
            vec!["with", "app", "srv", "--there", "--", "cmd", "a b"],
            r(RecipeCall {
                recipe: Some("srv".into()),
                there: true,
                command: Some("cmd 'a b'".into()),
                ..recipe(RecipeVerb::With, "app")
            }),
        ),
        (
            vec!["shell", "app", "--reason", "why", "-b", "--", "x y"],
            r(RecipeCall {
                reason: Some("why".into()),
                bench: true,
                command: Some("x y".into()),
                ..recipe(RecipeVerb::Shell, "app")
            }),
        ),
        (
            vec!["raw", "--reason", "why", "--", "ls"],
            r(RecipeCall {
                reason: Some("why".into()),
                command: Some("ls".into()),
                ..recipe(RecipeVerb::Raw, "")
            }),
        ),
        (vec!["list", "app"], r(recipe(RecipeVerb::List, "app"))),
        (vec!["runs"], r(recipe(RecipeVerb::Runs, ""))),
        (
            vec!["runs", "app/bench/x@gpu:0", "--all"],
            r(RecipeCall {
                all: true,
                ..recipe(RecipeVerb::Runs, "app/bench/x@gpu:0")
            }),
        ),
        (vec!["gaps"], r(recipe(RecipeVerb::Gaps, ""))),
        (
            vec!["batch", "-", "--verbose"],
            r(RecipeCall {
                verbose: true,
                ..recipe(RecipeVerb::Batch, "-")
            }),
        ),
        (
            vec!["batch", "steps@1.txt"],
            r(recipe(RecipeVerb::Batch, "steps@1.txt")),
        ),
        (
            vec!["machines", "--json"],
            r(RecipeCall {
                json: true,
                ..recipe(RecipeVerb::Machines, "")
            }),
        ),
        (vec!["build", "--help"], Invocation::RecipeHelp),
        (vec!["list", "-h"], Invocation::RecipeHelp),
        (vec!["build", "--version"], Invocation::Version),
    ]
}

fn refusals() -> Vec<(Vec<&'static str>, &'static str, bool)> {
    vec![
        (vec![], "no command given", true),
        (vec!["run"], "no command given", true),
        (vec!["--"], "no command given", true),
        (vec!["--on"], "dibs: --on needs a value.", false),
        (vec!["--on", ""], "dibs: --on needs a value.", false),
        (
            vec!["--days", "", "--gc"],
            "dibs: --days needs a value.",
            false,
        ),
        (
            vec!["--wait", "soon", "x"],
            "dibs: --wait takes seconds",
            false,
        ),
        (vec!["--max", "1h", "x"], "dibs: --max takes seconds", false),
        (vec!["--friction"], "dibs: --friction needs a value.", false),
        (
            vec!["--friction", ""],
            "dibs: --friction needs a value.",
            false,
        ),
        (
            vec!["--friction", "--reply", "x", "a"],
            "dibs: --reply needs an issue number, not x",
            false,
        ),
        (
            vec!["--friction", "--reply", "5", "a", "b"],
            "dibs: --reply <issue> '<answer>' takes only --close after it",
            false,
        ),
        (
            vec!["--friction", "--reply", "5", " "],
            "dibs: --reply needs the answer to post",
            false,
        ),
        (
            vec!["--bench", "build", "app"],
            "dibs: only --on can come before build. Put the rest after it:  dibs build ... <flags>",
            false,
        ),
        (
            vec!["--on", "m", "--on", "n", "runs"],
            "dibs: only --on can come before runs. Put the rest after it:  dibs runs ... <flags>",
            false,
        ),
        (vec!["build"], "dibs: needs a repo", false),
        (
            vec!["with", "app", "srv"],
            "dibs: with runs a command here against the repo's servers: dibs with <repo>[@<ref>] <service> -- <command>",
            false,
        ),
        (
            vec!["bench", "app", "--reps", "0"],
            "dibs: --reps needs a count",
            false,
        ),
        (
            vec!["bench", "app", "-x"],
            "dibs: unknown option: -x",
            false,
        ),
        (
            vec!["bench", "app", "--size", "--on"],
            "dibs: --size needs a value, or is not a flag dibs has",
            false,
        ),
        (
            vec!["bench", "app", "--sweep", "n"],
            "dibs: --sweep takes <name>=<value,value,...>, not n",
            false,
        ),
        (
            vec!["with", "app", "srv", "--"],
            "dibs: -- needs a command after it",
            false,
        ),
        (
            vec!["--with", "s=x", "--ready", "tcp:a:api", "y"],
            "dibs: --ready tcp:api names no port. Use a number, or --port api to have one picked.",
            false,
        ),
        (
            vec!["--sync", "--label", "l", "x", ":y"],
            "dibs: --label is a dibs flag, and after --sync everything is rsync's.\n  Put it before:  dibs --label ... --sync <opts> <src> <dst>",
            false,
        ),
        (
            vec!["--hold", "--sync", "-a", "x", ":y"],
            "dibs: --hold takes a lock for a command run on this computer. It goes alone or with --bench.",
            false,
        ),
        (
            vec!["--rsh", "host"],
            "--rsh is rsync's transport, not for calling directly",
            false,
        ),
    ]
}

/// The words of a title the refusal snapshot printed, quoted the way the suite quotes them.
fn split(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                word.extend(chars.by_ref().take_while(|c| *c != '\''));
            }
            '"' => {
                started = true;
                word.extend(chars.by_ref().take_while(|c| *c != '"'));
            }
            '\\' => {
                started = true;
                word.extend(chars.next());
            }
            ' ' if started => {
                words.push(std::mem::take(&mut word));
                started = false;
            }
            ' ' => {}
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}
