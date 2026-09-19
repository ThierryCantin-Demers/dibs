//! Files a run wrote, brought back to the caller.
//!
//! A step keeps what it wrote in its own job directory on the machine, so the files expire with
//! the job's log and are found by the same id. The caller fetches them without a lock, the way
//! `dibs out` reads a log.

/// `run`, then the files matching `patterns` copied into the job's directory at their path in the
/// tree, or under `target/` for one under `$CARGO_TARGET_DIR`. Only files newer than the job are
/// taken: a tree is reused from run to run, and a file an earlier run left would look current.
pub fn collecting(run: &str, patterns: &[String]) -> String {
    if patterns.is_empty() {
        return run.to_string();
    }
    format!(
        r#"( {run} ); __dibs_rc=$?
__dibs_ref=${{DIBS_SCRATCH:-$HOME/.cache/dibs}}/jobs/${{DIBS_JOB:-}}/cmd
if [ -n "${{DIBS_JOB:-}}" ] && [ -f "$__dibs_ref" ]; then
    __dibs_n=0
    shopt -s globstar nullglob
    for __dibs_f in {patterns}; do
        [ -f "$__dibs_f" ] && [ -n "$(find "$__dibs_f" -maxdepth 0 -newer "$__dibs_ref" 2>/dev/null)" ] || continue
        __dibs_rel=${{__dibs_f#"$CARGO_TARGET_DIR"/}}
        [ "$__dibs_rel" = "$__dibs_f" ] || __dibs_rel=target/$__dibs_rel
        mkdir -p "${{__dibs_ref%/cmd}}/artifacts/$(dirname "$__dibs_rel")" &&
            cp -p "$__dibs_f" "${{__dibs_ref%/cmd}}/artifacts/$__dibs_rel" && __dibs_n=$((__dibs_n + 1))
    done
    shopt -u globstar nullglob
    [ "$__dibs_n" = 0 ] || echo "DIBS-ARTIFACTS $__dibs_n"
fi
exit $__dibs_rc"#,
        patterns = patterns.join(" ")
    )
}

/// How many files a step kept, from its report.
pub fn kept(report: &str) -> Option<u32> {
    report.lines().filter_map(|l| l.strip_prefix("DIBS-ARTIFACTS ")).last().and_then(|n| n.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn a_step_keeps_the_files_it_wrote_and_none_an_earlier_run_left() {
        let root = std::env::temp_dir().join(format!("dibs-art-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (tree, target, job) = (root.join("tree"), root.join("target"), root.join("scratch/jobs/j1"));
        for d in [tree.join("results"), target.join("criterion/gemm"), job.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(tree.join("results/old.json"), "old").unwrap();
        Command::new("touch").arg("-d").arg("1 hour ago").arg(tree.join("results/old.json")).status().unwrap();
        std::fs::write(job.join("cmd"), "x").unwrap();
        let patterns = vec!["results/*.json".to_string(), "$CARGO_TARGET_DIR/criterion/**/estimates.json".to_string()];
        let run = "echo new > results/new.json; echo e > $CARGO_TARGET_DIR/criterion/gemm/estimates.json; exit 4";
        let out = Command::new("bash")
            .arg("-c")
            .arg(collecting(run, &patterns))
            .current_dir(&tree)
            .env("DIBS_SCRATCH", root.join("scratch"))
            .env("DIBS_JOB", "j1")
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(4), "the step's own exit: {text}");
        assert_eq!(kept(&text), Some(2), "{text}");
        let kept_dir = job.join("artifacts");
        assert!(kept_dir.join("results/new.json").exists());
        assert!(kept_dir.join("target/criterion/gemm/estimates.json").exists());
        assert!(!kept_dir.join("results/old.json").exists(), "left by an earlier run");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_changes_for_a_recipe_that_keeps_nothing() {
        assert_eq!(collecting("make", &[]), "make");
        assert_eq!(kept("DIBS-STATE x=1\n"), None);
    }
}
