use super::manifest::Recipe;
use std::collections::BTreeMap;

impl Recipe {
    /// The value of every parameter for one invocation: what was asked for, checked against
    /// what is declared, over the defaults.
    pub fn values(
        &self,
        given: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, String> {
        if let Some(unknown) = given.keys().find(|k| !self.params.contains_key(*k)) {
            let have: Vec<&str> = self.params.keys().map(|s| s.as_str()).collect();
            return Err(match have.is_empty() {
                true => {
                    format!("this recipe takes no parameters, so --{unknown} means nothing to it")
                }
                false => format!(
                    "no parameter '{unknown}'; this recipe takes: {}",
                    have.join(", ")
                ),
            });
        }
        let mut out = BTreeMap::new();
        for (name, p) in &self.params {
            let v = match given.get(name).or(p.default.as_ref()) {
                Some(v) => v.clone(),
                None => return Err(format!("--{name} has no default, so it has to be given")),
            };
            if !p.choices.is_empty() && !p.choices.contains(&v) {
                return Err(format!(
                    "--{name} {v} is not one of: {}",
                    p.choices.join(", ")
                ));
            }
            out.insert(name.clone(), v);
        }
        Ok(out)
    }

    /// The recipe as it will run, with `{name}` replaced in every command and every exported
    /// value. Only declared names are substituted: a command is shell, and `${VAR}`, `awk
    /// '{print $1}'` and `find -exec {}` all pass through untouched.
    pub fn bound(&self, values: &BTreeMap<String, String>) -> Recipe {
        let fill = |s: &str| {
            let mut out = s.to_string();
            for (k, v) in values {
                out = out.replace(&format!("{{{k}}}"), v);
            }
            out
        };
        let mut rec = self.clone();
        for st in &mut rec.steps {
            st.run = fill(&st.run);
            st.env = st.env.iter().map(|(k, v)| (k.clone(), fill(v))).collect();
        }
        rec.artifacts = rec.artifacts.iter().map(|a| fill(a)).collect();
        rec
    }
}
