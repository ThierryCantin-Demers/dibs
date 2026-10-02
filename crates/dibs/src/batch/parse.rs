use dibs::cli::{Invocation, Mode, RecipeCall, RecipeVerb, RunLock};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: String,
    /// Verbatim, so a failed step is rerun by copying it.
    pub line: String,
    pub after: Vec<String>,
    pub cont: bool,
    pub on: Option<String>,
    pub lock: &'static str,
    pub label: Option<String>,
    pub device: Option<String>,
    /// What a recipe step asks for, so the jobs it will make can be planned.
    pub recipe: Option<RecipeCall>,
}

pub fn parse(text: &str) -> Result<Vec<Step>, String> {
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
        let at = |e: String| format!("line {first_line}: {e}");
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
        return Err(format!(
            "line {first_line}: the last line ends in a backslash"
        ));
    }
    if steps.is_empty() {
        return Err("the batch has no steps".into());
    }
    let names: HashSet<&str> = steps.iter().map(|s| s.name.as_str()).collect();
    for s in &steps {
        for a in &s.after {
            if !names.contains(a.as_str()) {
                return Err(format!(
                    "step '{}' waits for '{a}', and no step has that name",
                    s.name
                ));
            }
        }
    }
    order(&steps)?;
    Ok(steps)
}

/// Fails on a cycle, naming a step in it.
pub(crate) fn order(steps: &[Step]) -> Result<Vec<usize>, String> {
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
                return Err(format!(
                    "step '{}' waits on itself through after=",
                    steps[stuck].name
                ));
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
pub(crate) fn describe(words: &[String]) -> Result<Step, String> {
    let read = Invocation::parse_unexpanded(&words[1..])
        .map_err(|e| e.message.trim_start_matches("dibs: ").to_string())?;
    let mut step = Step {
        name: String::new(),
        line: String::new(),
        after: Vec::new(),
        cont: false,
        on: None,
        lock: "shared",
        label: None,
        device: None,
        recipe: None,
    };
    match read {
        Invocation::Call(call) => {
            step.lock = match &call.mode {
                Mode::Run(run) if run.lock == RunLock::Bench => "bench",
                Mode::Peek(_) => "peek",
                Mode::Sync(_) => "sync",
                Mode::Watch { .. } => {
                    return Err("a batch step cannot --watch: it never finishes".into());
                }
                _ => "shared",
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
                step.lock = "recipe";
                step.label = Some(recipe_label(words));
                step.recipe = Some(call);
            }
        }
        _ => {}
    }
    Ok(step)
}

/// The verb and the two words after it, which only `--on <machine>` may come before.
pub(crate) fn recipe_label(words: &[String]) -> String {
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
    pub(crate) fn measures(&self) -> bool {
        self.lock == "bench"
            || (self.lock == "recipe"
                && self
                    .label
                    .as_deref()
                    .is_some_and(|l| l.starts_with("bench ")))
    }
}
