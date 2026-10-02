//! What was measured, as opposed to how.
//!
//! The recipe is the procedure and names no revisions; this is the event, and names all of
//! them. Together they are what makes a number worth keeping: the recipe stays runnable
//! against code written next year, and every historical result stays fully identified.
//!
//! dibs records the resolution, it does not perform it. These repos develop against each other
//! through local path dependencies, so which cubecl a cubek build saw is a property of the
//! working tree rather than a declaration anyone made. Reading it back is complete and costs
//! nothing; controlling it would be writing a package manager next to cargo.
//!
//! The reading happens on the machine, in the tree that was actually built. Doing it here
//! would report the commit of a checkout on this laptop, which is a different thing that
//! happens to share a name.

use dibs_format::{Pair, Pairs};

/// Printed by a measured step before it runs, as `DIBS-STATE key=value ...`: what the machine read
/// of itself as it took the lock, its platform's `machine_state`, since the same recipe on a
/// machine in another state is another history.
pub fn stated(run: &str) -> String {
    format!("echo \"DIBS-STATE ${{DIBS_STATE:-}}\"\n{run}")
}

/// The values a `DIBS-STATE` line carried, empty ones dropped.
pub fn state_of(report: &str) -> Pairs {
    report
        .lines()
        .filter_map(|l| l.strip_prefix("DIBS-STATE "))
        .next_back()
        .into_iter()
        .flat_map(|l| l.split_whitespace())
        .filter_map(|kv| kv.split_once('='))
        .filter(|(_, v)| !v.is_empty())
        .map(|(name, value)| Pair {
            name: name.to_string(),
            value: value.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_measured_step_says_what_state_the_machine_was_in_before_it_runs() {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(stated("echo RAN"))
            .env("DIBS_STATE", "kernel=6.8.0 nvidia=")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            state_of(&text)
                .iter()
                .any(|p| p.name == "kernel" && !p.value.is_empty()),
            "{text}"
        );
        assert!(
            text.find("DIBS-STATE").unwrap() < text.find("RAN").unwrap(),
            "{text}"
        );
    }

    // A machine with no NVIDIA driver has no version for it, which is not a version called "".
    #[test]
    fn a_value_the_machine_did_not_have_is_left_out() {
        let state = state_of("noise\nDIBS-STATE governor=performance kernel=6.8.0 nvidia=\n");
        assert_eq!(
            state,
            Pairs::from(vec![
                ("governor".to_string(), "performance".to_string()),
                ("kernel".into(), "6.8.0".into())
            ])
        );
    }
}
