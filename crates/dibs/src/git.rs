use std::{path::Path, process::Command};

/// git, run in one checkout.
pub struct Git<'a>(pub &'a Path);

impl Git<'_> {
    /// What the command printed, or what git said when it failed.
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.0)
            .args(args)
            .output()
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        if !out.status.success() {
            return Err(format!(
                "git {} in {}: {}",
                args.join(" "),
                self.0.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
