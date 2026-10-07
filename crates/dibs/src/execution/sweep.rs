use super::{base::sh, error::RunError, refs::sides};
use crate::{
    batch,
    cli::{RecipeCall, Sweep},
    recipe::resolve,
};
use std::{collections::BTreeMap, process::ExitCode};

/// Every combination `--sweep` asks for, each a complete set of values for one run. Without a
/// sweep this is the one point the call already described.
pub fn sweep_points(args: &RecipeCall) -> Vec<BTreeMap<String, String>> {
    let mut points = vec![args.params.clone()];
    for Sweep { name, values } in &args.sweep {
        points = points
            .iter()
            .flat_map(|p| {
                values.iter().map(|v| {
                    let mut q = p.clone();
                    q.insert(name.clone(), v.clone());
                    q
                })
            })
            .collect();
    }
    points
}

pub fn sweep_run(
    args: &RecipeCall,
    points: &[BTreeMap<String, String>],
) -> Result<ExitCode, RunError> {
    // Every point is checked before any of them is queued: a value the recipe refuses should be
    // found now, not two measurements into a sweep that is already holding the machine.
    sides(args.reference.as_deref())?;
    for p in points {
        let probe = RecipeCall {
            params: p.clone(),
            sweep: Vec::new(),
            reps: 1,
            ..args.clone()
        };
        resolve(&probe)?;
    }
    let text = sweep_text(args, points);
    let code = batch::run(
        &text,
        &batch::Options {
            dry_run: args.dry_run,
            verbose: args.verbose,
            on: args.machine(),
            owner: None,
        },
    )?;
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// The batch a sweep becomes: one ordinary dibs call per point, named by what makes it that
/// point, in the order the sweep was written.
pub fn sweep_text(args: &RecipeCall, points: &[BTreeMap<String, String>]) -> String {
    let target = match &args.reference {
        Some(r) => format!("{}@{r}", args.repo),
        None => args.repo.clone(),
    };
    let mut text = String::new();
    for p in points {
        let mut line = format!("dibs {} {}", args.verb, sh(&target));
        if let Some(r) = &args.recipe {
            line += &format!(" {r}");
        }
        if args.bench {
            line += " --bench";
        }
        if let Some(m) = args.max {
            line += &format!(" --max {m}");
        }
        if args.reps > 1 {
            line += &format!(" --reps {}", args.reps);
        }
        if let Some(d) = &args.artifacts_to {
            line += &format!(
                " --artifacts {}",
                sh(&format!("{d}/{}", point_name(args, p)))
            );
        }
        for pin in &args.pins {
            line += &format!(" --pin {}", sh(pin));
        }
        if args.anyway {
            line += " --anyway";
        }
        if args.new_series {
            line += " --new-series";
        }
        if let Some(d) = &args.device {
            line += &format!(" --device {d}");
        }
        if let Some(r) = &args.reason {
            line += &format!(" --reason {}", sh(r));
        }
        for (k, v) in p {
            line += &format!(" --{k} {}", sh(v));
        }
        if let Some(c) = &args.command {
            line += &format!(" -- {}", sh(c));
        }
        text += &format!("[{}] {line}\n", point_name(args, p));
    }
    text
}

/// What the summary calls one point: the values that make it that point.
fn point_name(args: &RecipeCall, p: &BTreeMap<String, String>) -> String {
    let slug = |v: &str| {
        v.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let parts: Vec<String> = args
        .sweep
        .iter()
        .map(|Sweep { name, .. }| {
            let value = p.get(name).map(String::as_str).unwrap_or("");
            format!("{name}-{}", slug(value))
        })
        .collect();
    parts.join(".")
}
