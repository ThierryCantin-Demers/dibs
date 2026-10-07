//! The `dibs` command: one grammar, then the recipe layer, a friction report, or a call.

use dibs::{
    batch,
    call::{Dispatch, Guard, MachineCall, Rsh},
    caller::Caller,
    cli::{Call, Help, Hook, Invocation, Mode, RecipeCall, RecipeVerb},
    execution::{self, Refusal, RunError},
    fleet,
    hook::SshHook,
    machine::Runner,
    paths::FileError,
    recipe,
    records::{Complaints, FrictionLog, RunLog},
    reports::{self, ReportsRepo},
    update::{Build, ChangeNotice},
};
use std::{path::Path, process::ExitCode};

fn main() -> ExitCode {
    let words: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = Runner::serves(&words) {
        return ExitCode::from(code.rem_euclid(256) as u8);
    }
    match dispatch(&words) {
        Ok(code) => code,
        Err(e) => {
            eprint!("{e}");
            ExitCode::from(e.exit())
        }
    }
}

fn dispatch(words: &[String]) -> Result<ExitCode, RunError> {
    // Words outside the grammar: the background fetch of report replies, the build stamp, and
    // the processes dibs starts as rsync's transport and as the guard of a held command or a
    // batch step.
    match words {
        [verb, flag, into] if verb == "friction" && flag == "--replies" => {
            return friction_replies(Path::new(into));
        }
        [flag] if flag == "--version" => return Ok(version()),
        [word, rest @ ..] if word == Rsh::WORD => {
            return Ok(ExitCode::from(Rsh::serve(rest).rem_euclid(256) as u8));
        }
        [word, at, command @ ..] if word == Guard::WORD => {
            return Ok(ExitCode::from(
                Guard::serve(at, command).rem_euclid(256) as u8
            ));
        }
        [word, line] if word == batch::StepGuard::WORD => {
            return Ok(ExitCode::from(
                batch::StepGuard::serve(line).rem_euclid(256) as u8,
            ));
        }
        _ => {}
    }
    let invocation = match Invocation::parse(words) {
        Ok(invocation) => invocation,
        Err(e) => {
            eprintln!("{e}");
            if e.with_help {
                print!("{}", Help::text());
            }
            return Ok(ExitCode::from(e.exit()));
        }
    };
    match invocation {
        Invocation::Help => {
            print!("{}", Help::text());
            Ok(ExitCode::SUCCESS)
        }
        Invocation::Version => Ok(version()),
        Invocation::Friction(friction) => Ok(reports::friction_verb(friction)?),
        Invocation::Hook(Hook::Ssh) => Ok(ExitCode::from(SshHook::serve() as u8)),
        Invocation::Recipe(call) => {
            let caller = Caller::from_env();
            ChangeNotice::tell_once(&caller);
            reports::Notice { caller: &caller }.tell();
            run(call, &caller)
        }
        Invocation::Call(call) => {
            let caller = Caller::from_env();
            if call.mode != Mode::Update {
                ChangeNotice::tell_once(&caller);
                reports::Notice { caller: &caller }.tell();
            }
            let code = Dispatch {
                call: &call,
                caller: &caller,
            }
            .exit();
            Ok(ExitCode::from(code.rem_euclid(256) as u8))
        }
    }
}

/// Stamped at build time, so a binary that has drifted from the source can be told apart.
fn version() -> ExitCode {
    println!(
        "dibs {} ({})",
        env!("CARGO_PKG_VERSION"),
        Build::COMMIT.unwrap_or("commit unknown")
    );
    ExitCode::SUCCESS
}

fn run(args: RecipeCall, caller: &Caller) -> Result<ExitCode, RunError> {
    if args.there && args.verb != RecipeVerb::With {
        return Err(Refusal::There.into());
    }
    // Refused even by a verb that never reaches a machine, as every call naming one is.
    let on = Call {
        on: args.machine(),
        ..Call::default()
    };
    MachineCall::new(&on, caller)?;

    if args.verb == RecipeVerb::Batch {
        let text = match args.repo.as_str() {
            "-" => std::io::read_to_string(std::io::stdin()).map_err(Refusal::BatchStdin)?,
            path => std::fs::read_to_string(path).map_err(FileError::at(Path::new(path)))?,
        };
        let code = batch::run(
            &text,
            &batch::Options {
                dry_run: args.dry_run,
                verbose: args.verbose,
                on: args.machine(),
                owner: Some(caller.id.clone()),
            },
        )?;
        return Ok(ExitCode::from(code.clamp(0, 255) as u8));
    }

    if args.verb == RecipeVerb::Machines {
        let only = (!args.repo.is_empty()).then_some(args.repo.as_str());
        return Ok(fleet::command(
            args.json,
            only,
            &recipe::root_of(&args)?,
            fleet::recipe_repos(),
            &fleet::pool()?,
        )?);
    }

    if args.verb == RecipeVerb::Gaps {
        print!("{}", RunLog::here()?.runs()?.gaps());
        let notes = FrictionLog::here()?.notes();
        print!("{}", Complaints::of(&notes));
        return Ok(ExitCode::SUCCESS);
    }

    if args.verb == RecipeVerb::Raw {
        return execution::raw(&args);
    }

    // Reads only what this machine recorded, so it needs no repo and no connection.
    if args.verb == RecipeVerb::Runs {
        let label = if args.repo.is_empty() {
            None
        } else {
            Some(args.repo.as_str())
        };
        let runs = RunLog::here()?.runs()?;
        print!("{}", runs.report(label, 30, args.all));
        return Ok(ExitCode::SUCCESS);
    }

    if args.verb == RecipeVerb::With {
        return execution::with_service(&args);
    }

    if args.verb == RecipeVerb::List {
        return Ok(recipe::list(&args)?);
    }

    execution::run_recipe(args)
}

/// Answers to the reports `DIBS_FRICTION_BY` filed, kept for its next call.
fn friction_replies(into: &Path) -> Result<ExitCode, RunError> {
    let Some(repo) = ReportsRepo::from_env() else {
        return Ok(ExitCode::SUCCESS);
    };
    let by = std::env::var("DIBS_FRICTION_BY").unwrap_or_default();
    let notes = FrictionLog::here()?.notes();
    repo.fetch_replies(&notes, &by, into)?;
    Ok(ExitCode::SUCCESS)
}
