//! The `dibs` command: one grammar, then the recipe layer, a friction report, or a call.

use dibs::{
    batch,
    call::{Dispatch, Guard, MachineCall, Rsh},
    caller::Caller,
    cli::{Call, Help, Invocation, Mode, RecipeCall, RecipeVerb},
    execution::{self, Refusal, RunError},
    fleet,
    machine::{RUNNER_WORD, Runner},
    paths::FileError,
    recipe,
    records::{self, friction, runs},
    reports,
    update::{Build, ChangeNotice},
};
use dibs_runner::Source;
use std::{path::Path, process::ExitCode};

fn main() -> ExitCode {
    let words: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&words) {
        Ok(code) => code,
        Err(e) => {
            eprint!("{e}");
            ExitCode::from(e.exit())
        }
    }
}

fn dispatch(words: &[String]) -> Result<ExitCode, RunError> {
    // Words outside the grammar: the background fetch of report replies, the build stamp, the
    // runner this binary links, and the processes dibs starts as rsync's transport and as the
    // guard of a held command or a batch step.
    match words {
        [verb, flag, into] if verb == "friction" && flag == "--replies" => {
            return friction_replies(Path::new(into));
        }
        [flag] if flag == "--version" => return Ok(version()),
        [word, rest @ ..] if word == RUNNER_WORD => {
            return Ok(ExitCode::from(
                dibs_runner::main(rest, Source { hash: Runner::HASH }).rem_euclid(256) as u8,
            ));
        }
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
        print!("{}", runs::gaps(&runs::load(&records::runs_path()?)?));
        print!("{}", friction::report(&friction::load(&friction::path()?)));
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
        let records = runs::load(&records::runs_path()?)?;
        print!("{}", runs::report(&records, label, 30, args.all));
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
    let Some(repo) = reports::repo() else {
        return Ok(ExitCode::SUCCESS);
    };
    let by = std::env::var("DIBS_FRICTION_BY").unwrap_or_default();
    let notes = friction::load(&friction::path()?);
    reports::fetch_replies(&repo, &notes, &by, into)?;
    Ok(ExitCode::SUCCESS)
}
