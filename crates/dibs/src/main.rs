//! The `dibs` command: one grammar, then the recipe layer, a friction report, or a call.

mod artifacts;
mod batch;
mod execution;
mod fleet;
mod git;
mod gitdeps;
mod lockfile;
mod provenance;
mod recipe;
mod records;
mod reports;
mod worktree;

use dibs::{
    call::{Dispatch, Guard, Rsh},
    caller::Caller,
    cli::{Help, Invocation, Mode, RecipeCall, RecipeVerb},
    paths::Paths,
    update::ChangeNotice,
};
use execution::RunError;
use records::{friction, runs};
use std::{path::Path, process::ExitCode};

fn main() -> ExitCode {
    let words: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&words) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("dibs: {e}");
            ExitCode::from(e.exit())
        }
    }
}

fn dispatch(words: &[String]) -> Result<ExitCode, RunError> {
    // Words outside the grammar: the background fetch of report replies, the build stamp, and
    // the processes dibs starts as rsync's transport and as a held command's guard.
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
        Invocation::Friction(friction) => reports::friction_verb(friction),
        Invocation::Recipe(call) => {
            let caller = Caller::from_env();
            change_notice(&caller);
            if call.verb == RecipeVerb::Batch {
                // SAFETY: nothing has started a thread yet.
                unsafe { std::env::set_var("DIBS_BATCH_OWNER", &caller.id) };
            }
            run(call)
        }
        Invocation::Call(call) => {
            let caller = Caller::from_env();
            if call.mode != Mode::Update {
                change_notice(&caller);
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

/// Stamped by install.sh, so a binary that has drifted from the source can be told apart.
fn version() -> ExitCode {
    println!(
        "dibs {} ({})",
        env!("CARGO_PKG_VERSION"),
        option_env!("DIBS_CORE_COMMIT").unwrap_or("commit unknown")
    );
    ExitCode::SUCCESS
}

fn change_notice(caller: &Caller) {
    if let Some(seen) = Paths::from_env().seen() {
        ChangeNotice::of_this_build(seen).tell(caller);
    }
}

fn run(args: RecipeCall) -> Result<ExitCode, RunError> {
    if args.there && args.verb != RecipeVerb::With {
        return Err(
            "--there belongs to with: it runs the command on the machine beside the repo's servers"
                .into(),
        );
    }
    // Every call this makes, and every step of a batch, reads the machine from here.
    if let Some(m) = &args.on {
        // SAFETY: nothing has started a thread yet; every thread this spawns comes after.
        unsafe { std::env::set_var("DIBS_ON", m) };
    }

    if args.verb == RecipeVerb::Batch {
        let text = match args.repo.as_str() {
            "-" => std::io::read_to_string(std::io::stdin())
                .map_err(|e| format!("reading the batch from stdin: {e}"))?,
            path => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
        };
        let code = batch::run(
            &text,
            &batch::Options {
                dry_run: args.dry_run,
                verbose: args.verbose,
            },
        )?;
        return Ok(ExitCode::from(code.clamp(0, 255) as u8));
    }

    if args.verb == RecipeVerb::Machines {
        let only = (!args.repo.is_empty()).then_some(args.repo.as_str());
        return Ok(fleet::command(
            args.json,
            only,
            &recipe::root_of(&args),
            fleet::recipe_repos(),
            &fleet::pool(),
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
