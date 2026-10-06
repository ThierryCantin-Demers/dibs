use crate::probe::{base::Report, facts::Tools};

/// A Mac's one GPU, as system_profiler names it. There is no slot to pin it by: the alias names it
/// for the record, and `--device` on it pins nothing.
pub struct Gpus {
    name: Option<String>,
}

impl Gpus {
    pub fn find(report: &mut Report) -> Gpus {
        let said = Tools::here()
            .output("system_profiler", &["SPDisplaysDataType"])
            .unwrap_or_default();
        let field = |key: &str| {
            said.lines()
                .find_map(|l| Some(l.trim().strip_prefix(key)?.trim().to_string()))
        };
        let name = field("Chipset Model:");
        match &name {
            Some(name) => report.line(&format!(
                "    gpu   {name}, {} cores, {}",
                field("Total Number of Cores:").unwrap_or_else(|| "?".into()),
                field("Metal Support:").unwrap_or_else(|| "Metal".into())
            )),
            None => report.warn("system_profiler lists no GPU"),
        }
        Gpus { name }
    }

    pub fn entries(&self) -> String {
        let Some(name) = &self.name else {
            return String::new();
        };
        let alias: String = name
            .to_lowercase()
            .replace("apple", "")
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            .collect();
        format!(
            "\n  [[machine.@NAME@.device]]\n  kind     = \"gpu\"\n  alias    = \"gpu:{alias}\"\n  name     = \"{name}\"\n  runtimes = [\"metal\"]\n"
        )
    }
}
