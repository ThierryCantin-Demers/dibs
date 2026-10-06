use crate::cli::{Invocation, Mode, RecipeCall, RecipeVerb, RunLock};
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

/// Why a batch is refused before anything runs, or stops: said as its `Display`.
#[derive(Debug)]
pub enum BatchError {
    /// A line that is not one dibs call the driver can run.
    Line {
        line: usize,
        why: String,
    },
    NoSteps,
    UnknownStep {
        step: String,
        waits_for: String,
    },
    /// A step waits on itself through the steps it waits for.
    Cycle {
        step: String,
    },
    /// A measurement names no machine, and a measurement is never placed.
    Unplaced {
        step: String,
    },
    Refused(String),
}

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BatchError::Line { line, why } => write!(f, "line {line}: {why}"),
            BatchError::NoSteps => f.write_str("the batch has no steps"),
            BatchError::UnknownStep { step, waits_for } => write!(
                f,
                "step '{step}' waits for '{waits_for}', and no step has that name"
            ),
            BatchError::Cycle { step } => {
                write!(f, "step '{step}' waits on itself through after=")
            }
            BatchError::Unplaced { step } => write!(
                f,
                "step {step} measures and names no machine, and a measurement is never placed for you. Give it\n  \
                 --on <machine>, or export DIBS_ON=<machine> before the batch to cover every step."
            ),
            BatchError::Refused(why) => f.write_str(why),
        }
    }
}

impl From<String> for BatchError {
    fn from(why: String) -> BatchError {
        BatchError::Refused(why)
    }
}

/// What a step of a batch is, by the lock its call takes: a recipe takes several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Shared,
    Bench,
    Peek,
    Sync,
    Recipe,
}

impl StepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Shared => "shared",
            StepKind::Bench => "bench",
            StepKind::Peek => "peek",
            StepKind::Sync => "sync",
            StepKind::Recipe => "recipe",
        }
    }
}

impl fmt::Display for StepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: String,
    /// Verbatim, so a failed step is rerun by copying it.
    pub line: String,
    pub after: Vec<String>,
    pub cont: bool,
    pub on: Option<String>,
    pub lock: StepKind,
    pub label: Option<String>,
    pub device: Option<String>,
    /// What a recipe step asks for, so the jobs it will make can be planned.
    pub recipe: Option<RecipeCall>,
}

pub fn parse(text: &str) -> Result<Vec<Step>, BatchError> {
    let mut steps: Vec<Step> = Vec::new();
    let mut joined = String::new();
    let mut first_line = 0;
    for (i, raw) in text.lines().enumerate() {
        if joined.is_empty() {
            first_line = i + 1;
        }
        if let Some(head) = raw.strip_suffix('\\') {
            joined.push_str(head);
            joined.push(' ');
            continue;
        }
        joined.push_str(raw);
        let line = std::mem::take(&mut joined);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |why: String| BatchError::Line {
            line: first_line,
            why,
        };
        let (attrs, command) = match line.strip_prefix('[') {
            Some(rest) => {
                let (a, c) = rest
                    .split_once(']')
                    .ok_or_else(|| at("an attribute list is not closed with ]".into()))?;
                (a.trim(), c.trim())
            }
            None => ("", line),
        };
        let mut name = None;
        let mut after = None;
        let mut cont = false;
        for attr in attrs.split_whitespace() {
            if attr == "cont" {
                cont = true;
            } else if let Some(v) = attr.strip_prefix("after=") {
                after = Some(
                    v.split(',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>(),
                );
            } else if !attr.contains('=') && name.is_none() {
                name = Some(attr.to_string());
            } else {
                return Err(at(format!(
                    "unknown attribute '{attr}': a step takes a name, after=a,b and cont"
                )));
            }
        }
        let words = split_words(command).map_err(at)?;
        if words.first().map(String::as_str) != Some("dibs") {
            return Err(at(format!(
                "'{command}' is not a dibs command. A batch is a list of dibs calls, one per line"
            )));
        }
        let s = describe(&words).map_err(at)?;
        let name = name.unwrap_or_else(|| (steps.len() + 1).to_string());
        if steps.iter().any(|p| p.name == name) {
            return Err(at(format!("a second step is named '{name}'")));
        }
        // A step says what it waits for, or waits for the one before it: sequential unless
        // told otherwise, so a list written top to bottom runs top to bottom.
        let after = after.unwrap_or_else(|| {
            steps
                .last()
                .map(|p| vec![p.name.clone()])
                .unwrap_or_default()
        });
        steps.push(Step {
            name,
            line: command.to_string(),
            after,
            cont,
            ..s
        });
    }
    if !joined.trim().is_empty() {
        return Err(BatchError::Line {
            line: first_line,
            why: "the last line ends in a backslash".into(),
        });
    }
    if steps.is_empty() {
        return Err(BatchError::NoSteps);
    }
    let names: HashSet<&str> = steps.iter().map(|s| s.name.as_str()).collect();
    for s in &steps {
        for a in &s.after {
            if !names.contains(a.as_str()) {
                return Err(BatchError::UnknownStep {
                    step: s.name.clone(),
                    waits_for: a.clone(),
                });
            }
        }
    }
    order(&steps)?;
    Ok(steps)
}

/// Fails on a cycle, naming a step in it.
pub fn order(steps: &[Step]) -> Result<Vec<usize>, BatchError> {
    let index: HashMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let mut done = vec![false; steps.len()];
    let mut out = Vec::new();
    while out.len() < steps.len() {
        let next = (0..steps.len())
            .find(|&i| !done[i] && steps[i].after.iter().all(|a| done[index[a.as_str()]]));
        match next {
            Some(i) => {
                done[i] = true;
                out.push(i);
            }
            None => {
                let stuck = (0..steps.len()).find(|&i| !done[i]).unwrap();
                return Err(BatchError::Cycle {
                    step: steps[stuck].name.clone(),
                });
            }
        }
    }
    Ok(out)
}

/// Shell words without expansion, for reading a step's flags. The step itself runs through bash,
/// so `$DIBS_BATCH` and quoting mean what they mean at a prompt. Anything that would make the
/// line more than one dibs call is refused.
pub fn split_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                        None => return Err("a single quote is not closed".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(x @ ('"' | '\\' | '$' | '`')) => cur.push(x),
                            Some(x) => {
                                cur.push('\\');
                                cur.push(x);
                            }
                            None => return Err("a double quote is not closed".into()),
                        },
                        Some(x) => cur.push(x),
                        None => return Err("a double quote is not closed".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(x) = chars.next() {
                    cur.push(x);
                }
            }
            ';' | '&' | '|' | '<' | '>' | '(' | ')' | '`' => {
                return Err(format!(
                    "'{c}' outside quotes makes this more than one dibs call. Quote the command you are sending"
                ));
            }
            '$' if chars.peek() == Some(&'(') => {
                return Err(
                    "$( outside quotes runs a command here, not on the machine. Quote it".into(),
                );
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

/// What the driver needs to know about a step before it runs: where it goes, for ordering, and
/// what it is, for the summary. The words are read as written, since bash expands them only
/// when the step runs; everything else passes through untouched.
pub fn describe(words: &[String]) -> Result<Step, String> {
    let read = Invocation::parse_unexpanded(&words[1..])
        .map_err(|e| e.message.trim_start_matches("dibs: ").to_string())?;
    let mut step = Step {
        name: String::new(),
        line: String::new(),
        after: Vec::new(),
        cont: false,
        on: None,
        lock: StepKind::Shared,
        label: None,
        device: None,
        recipe: None,
    };
    match read {
        Invocation::Call(call) => {
            step.lock = match &call.mode {
                Mode::Run(run) if run.lock == RunLock::Bench => StepKind::Bench,
                Mode::Peek(_) => StepKind::Peek,
                Mode::Sync(_) => StepKind::Sync,
                Mode::Watch { .. } => {
                    return Err("a batch step cannot --watch: it never finishes".into());
                }
                _ => StepKind::Shared,
            };
            step.on = call.on.map(|m| m.as_str().to_string());
            step.label = call.label.map(|l| l.as_str().to_string());
            step.device = call.device.map(|d| d.as_str().to_string());
        }
        Invocation::Recipe(call) => {
            if call.verb == RecipeVerb::Batch {
                return Err("a batch step cannot be a batch".into());
            }
            step.on = call.on.clone();
            step.device = call.device.clone();
            if call.verb.runs_jobs() {
                step.lock = StepKind::Recipe;
                step.label = Some(recipe_label(words));
                step.recipe = Some(call);
            }
        }
        _ => {}
    }
    Ok(step)
}

/// The verb and the two words after it, which only `--on <machine>` may come before.
pub fn recipe_label(words: &[String]) -> String {
    let verb = match words.get(1).map(String::as_str) {
        Some("--on") => 3,
        _ => 1,
    };
    words[verb.min(words.len())..]
        .iter()
        .take(3)
        .filter(|w| !w.starts_with('-'))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Step {
    pub fn measures(&self) -> bool {
        self.lock == StepKind::Bench
            || (self.lock == StepKind::Recipe
                && self
                    .label
                    .as_deref()
                    .is_some_and(|l| l.starts_with("bench ")))
    }
}
