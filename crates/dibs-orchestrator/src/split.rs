//! A recipe built on the compile pool and run on the GPU pool: what `splits/<repo>.toml` says,
//! the values it takes, and the steps a submission of it is planned as.

use crate::{inventory::Inventory, settings::Settings, store::Planned};
use dibs_format::{orchestrator::SplitCall, wire::BundleMake};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    split: BTreeMap<String, Split>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Split {
    /// What the measurement's series is filed under.
    pub label: String,
    /// The card it is measured on, by its alias.
    pub device: String,
    /// Run in the tree before the build, to refuse a tree the recipe cannot build.
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, Param>,
    pub build: Build,
    pub run: Run,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Param {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub choices: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub package: String,
    /// Cargo's target kind: `bin` or `example`.
    pub kind: String,
    pub name: String,
    /// `cargo`'s arguments, `{param}` substituted, split on whitespace.
    pub cargo: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Run where `$DIBS_BUNDLE` is the relayed bundle's directory.
    pub command: String,
}

/// A split recipe with its values given: everything the worker needs, stored with the plan.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct Resolved {
    pub repo: String,
    pub recipe: String,
    pub label: String,
    pub device: String,
    pub compile: String,
    pub gpu: String,
    pub check: Option<String>,
    pub package: String,
    pub kind: String,
    pub name: String,
    pub cargo: Vec<String>,
    pub rustflags: String,
    pub profile: String,
    #[serde(default)]
    pub image: Option<String>,
    pub env: BTreeMap<String, String>,
    pub command: String,
    pub values: BTreeMap<String, String>,
}

impl Resolved {
    pub fn plan(&self) -> Vec<Planned> {
        let on = |m: &str| Some(m.to_string());
        vec![
            Planned {
                name: "build".into(),
                machine: on(&self.compile),
            },
            Planned {
                name: "relay".into(),
                machine: None,
            },
            Planned {
                name: "stage".into(),
                machine: on(&self.gpu),
            },
            Planned {
                name: "measure".into(),
                machine: on(&self.gpu),
            },
        ]
    }

    /// The build's command, as the compile machine's shell runs it in the tree.
    pub fn build_command(&self) -> String {
        let cargo = self
            .cargo
            .iter()
            .map(|w| shell_word(w))
            .collect::<Vec<_>>()
            .join(" ");
        let build = format!(
            "cargo {cargo} --message-format=json-render-diagnostics > \"${}\"",
            BundleMake::REPORT
        );
        match &self.check {
            Some(check) => format!("{{ {check} ; }} && {build}"),
            None => build,
        }
    }

    pub fn what(&self) -> String {
        let values: String = self
            .values
            .iter()
            .map(|(k, v)| format!(" --{k} {v}"))
            .collect();
        format!(
            "split {}@local {}{values}: build on {}, measure on {} {}",
            self.repo, self.recipe, self.compile, self.gpu, self.device
        )
    }
}

/// Why a split submission is refused before it is stored.
#[derive(Debug)]
pub struct Refusal(pub String);

pub fn resolve(
    call: &SplitCall,
    text: Option<&str>,
    settings: &Settings,
    pools: &BTreeMap<String, Vec<String>>,
    inventory: &Inventory,
) -> Result<Resolved, Refusal> {
    let text = text.ok_or_else(|| {
        Refusal(format!(
            "the orchestrator has no split recipes for {}",
            call.repo
        ))
    })?;
    let file: File = toml::from_str(text)
        .map_err(|e| Refusal(format!("splits/{}.toml cannot be read: {e}", call.repo)))?;
    let split = file.split.get(&call.recipe).ok_or_else(|| {
        let known: Vec<&str> = file.split.keys().map(String::as_str).collect();
        Refusal(format!(
            "{} has no split recipe {}; it has {}",
            call.repo,
            call.recipe,
            known.join(", ")
        ))
    })?;
    let values = values(split, &call.values)?;
    let fill = |text: &str| fill(text, &values);
    let compile = pools
        .get(&settings.compile)
        .and_then(|m| m.first())
        .cloned()
        .ok_or_else(|| Refusal(format!("pool {} has no machine", settings.compile)))?;
    let gpu = pools
        .get(&settings.gpu)
        .into_iter()
        .flatten()
        .find(|m| inventory.has_card(m, &split.device))
        .cloned()
        .ok_or_else(|| {
            Refusal(format!(
                "no machine of pool {} has a card {}",
                settings.gpu, split.device
            ))
        })?;
    Ok(Resolved {
        repo: call.repo.clone(),
        recipe: call.recipe.clone(),
        label: split.label.clone(),
        device: split.device.clone(),
        compile,
        gpu,
        check: split.check.as_deref().map(fill),
        package: split.build.package.clone(),
        kind: split.build.kind.clone(),
        name: split.build.name.clone(),
        cargo: fill(&split.build.cargo)
            .split_whitespace()
            .map(String::from)
            .collect(),
        rustflags: settings.profile.rustflags.clone(),
        profile: settings.profile.name.clone(),
        image: settings.profile.image.clone(),
        env: split
            .run
            .env
            .iter()
            .map(|(k, v)| (k.clone(), fill(v)))
            .collect(),
        command: fill(&split.run.command),
        values,
    })
}

fn values(
    split: &Split,
    given: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, Refusal> {
    if let Some(unknown) = given.keys().find(|k| !split.params.contains_key(*k)) {
        return Err(Refusal(format!(
            "the recipe takes no --{unknown}; it takes {}",
            split
                .params
                .keys()
                .map(|k| format!("--{k}"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let mut values = BTreeMap::new();
    for (name, param) in &split.params {
        let value = given
            .get(name)
            .or(param.default.as_ref())
            .cloned()
            .ok_or_else(|| Refusal(format!("the recipe needs --{name}")))?;
        if !param.choices.is_empty() && !param.choices.contains(&value) {
            return Err(Refusal(format!(
                "--{name} is one of {}, not {value}",
                param.choices.join(", ")
            )));
        }
        if value.contains(['\'', '\n', '\0']) {
            return Err(Refusal(format!(
                "--{name} {value:?} holds a quote or a line break"
            )));
        }
        values.insert(name.clone(), value);
    }
    Ok(values)
}

fn fill(text: &str, values: &BTreeMap<String, String>) -> String {
    values.iter().fold(text.to_string(), |text, (k, v)| {
        text.replace(&format!("{{{k}}}"), v)
    })
}

/// `word` as one shell word.
pub fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b));
    match plain {
        true => word.to_string(),
        false => format!("'{}'", word.replace('\'', r"'\''")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPLITS: &str = r#"
[split.epoch]
label = "epoch"
device = "gpu:card"
check = "test -e src"
[split.epoch.params]
backend = { choices = ["cuda", "vulkan"], default = "cuda" }
epochs = {}
[split.epoch.build]
package = "app"
kind = "example"
name = "app"
cargo = "build --release -p app --example app --features {backend}"
[split.epoch.run]
env = { EPOCHS = "{epochs}" }
command = "$DIBS_BUNDLE/app"
"#;

    fn resolve_with(values: &[(&str, &str)]) -> Result<Resolved, Refusal> {
        let call = SplitCall {
            repo: "app".into(),
            recipe: "epoch".into(),
            values: values
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let pools = BTreeMap::from([
            ("compile".to_string(), vec!["box-a".to_string()]),
            ("gpu".to_string(), vec!["box-b".to_string()]),
        ]);
        let inventory = Inventory::parse(
            "[machine.box-b]\nssh = \"x\"\n[[machine.box-b.device]]\nkind = \"gpu\"\nalias = \"gpu:card\"\n",
        );
        resolve(
            &call,
            Some(SPLITS),
            &Settings::default(),
            &pools,
            &inventory,
        )
    }

    #[test]
    fn a_split_takes_its_values_and_finds_its_machines() {
        let resolved = resolve_with(&[("epochs", "1")]).unwrap();
        assert_eq!(resolved.compile, "box-a");
        assert_eq!(resolved.gpu, "box-b");
        assert_eq!(
            resolved.build_command(),
            "{ test -e src ; } && cargo build --release -p app --example app --features cuda --message-format=json-render-diagnostics > \"$DIBS_CARGO_REPORT\""
        );
        assert_eq!(resolved.env["EPOCHS"], "1");
        assert_eq!(
            resolved
                .plan()
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["build", "relay", "stage", "measure"]
        );
    }

    #[test]
    fn a_split_refuses_values_it_does_not_take() {
        assert!(resolve_with(&[]).unwrap_err().0.contains("needs --epochs"));
        assert!(
            resolve_with(&[("epochs", "1"), ("backend", "metal")])
                .unwrap_err()
                .0
                .contains("one of cuda, vulkan")
        );
        assert!(
            resolve_with(&[("epochs", "1"), ("speed", "2")])
                .unwrap_err()
                .0
                .contains("no --speed")
        );
    }
}
