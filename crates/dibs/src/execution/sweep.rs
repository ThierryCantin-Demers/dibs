use super::{error::RunError, refs::Side};
use crate::{
    batch,
    cli::{RecipeCall, ShellWord, Sweep},
    recipe::Resolved,
};
use std::{collections::BTreeMap, process::ExitCode};

/// A call and every combination its `--sweep` asks for.
pub struct SweepPoints<'a> {
    args: &'a RecipeCall,
    /// Each a complete set of values for one run. Without a sweep, the one point the call
    /// already described.
    pub points: Vec<BTreeMap<String, String>>,
}

impl<'a> SweepPoints<'a> {
    pub fn of(args: &'a RecipeCall) -> SweepPoints<'a> {
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
        SweepPoints { args, points }
    }

    pub fn run(&self) -> Result<ExitCode, RunError> {
        let args = self.args;
        // Every point is checked before any of them is queued: a value the recipe refuses should be
        // found now, not two measurements into a sweep that is already holding the machine.
        Side::list(args.reference.as_deref())?;
        for p in &self.points {
            let probe = RecipeCall {
                params: p.clone(),
                sweep: Vec::new(),
                reps: 1,
                ..args.clone()
            };
            Resolved::of(&probe)?;
        }
        let text = self.text();
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
    pub fn text(&self) -> String {
        let args = self.args;
        let target = match &args.reference {
            Some(r) => format!("{}@{r}", args.repo),
            None => args.repo.clone(),
        };
        let mut text = String::new();
        for p in &self.points {
            let mut line = format!("dibs {} {}", args.verb, ShellWord(&target));
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
                    ShellWord(&format!("{d}/{}", self.point_name(p)))
                );
            }
            for pin in &args.pins {
                line += &format!(" --pin {}", ShellWord(pin));
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
                line += &format!(" --reason {}", ShellWord(r));
            }
            for (k, v) in p {
                line += &format!(" --{k} {}", ShellWord(v));
            }
            if let Some(c) = &args.command {
                line += &format!(" -- {}", ShellWord(c));
            }
            text += &format!("[{}] {line}\n", self.point_name(p));
        }
        text
    }

    /// What the summary calls one point: the values that make it that point.
    fn point_name(&self, p: &BTreeMap<String, String>) -> String {
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
        let parts: Vec<String> = self
            .args
            .sweep
            .iter()
            .map(|Sweep { name, .. }| {
                let value = p.get(name).map(String::as_str).unwrap_or("");
                format!("{name}-{}", slug(value))
            })
            .collect();
        parts.join(".")
    }
}
