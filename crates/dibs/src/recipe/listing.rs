use super::{
    error::RecipeError,
    manifest::{Manifest, Source, Verb},
    repo::Checkouts,
};
use crate::{cli::RecipeCall, execution::Repo};
use std::process::ExitCode;

/// `dibs list <repo>`: its recipes, what each takes, and its services.
pub fn list(args: &RecipeCall) -> Result<ExitCode, RecipeError> {
    let dir = Checkouts::of(args)?.find(&args.repo)?;
    let manifest = Manifest::load(&dir, &Repo(&dir).identity())?;
    for v in [Verb::Bench, Verb::Build, Verb::Test] {
        let listing = manifest.listing(v);
        if !listing.is_empty() {
            println!("{}:", v.as_str());
            for (n, src) in listing {
                match src {
                    Source::Repo => println!("  {n}   (from the repo)"),
                    Source::Local => println!("  {n}"),
                }
                // What it accepts, so the valid invocations can be read off rather than
                // reconstructed from the recipe file.
                for (p, spec) in manifest
                    .recipe(v, n)
                    .map(|r| &r.params)
                    .into_iter()
                    .flatten()
                {
                    let choices = match spec.choices.is_empty() {
                        true => String::new(),
                        false => format!("  one of {}", spec.choices.join(", ")),
                    };
                    match &spec.default {
                        Some(d) => println!("      --{p} {d}{choices}"),
                        None => println!("      --{p} <value>, required{choices}"),
                    }
                }
                if let Some(r) = manifest.recipe(v, n).filter(|r| !r.fresh.is_empty()) {
                    println!("      fresh each run: {}", r.fresh.join(", "));
                }
            }
        }
    }
    let services = manifest.service_listing();
    if !services.is_empty() {
        println!("service:");
        for (n, src) in services {
            match src {
                Source::Repo => println!("  {n}   (from the repo)"),
                Source::Local => println!("  {n}"),
            }
        }
    }
    if !manifest.tree_fresh().is_empty() {
        println!(
            "\na new tree starts without: {}",
            manifest.tree_fresh().join(", ")
        );
    }
    println!("\nlocal recipes: {}", Manifest::local_dir().display());
    Ok(ExitCode::SUCCESS)
}
