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

use std::fmt::Write as _;

pub struct StepRecord {
    pub lock: &'static str,
    pub status: i32,
    pub seconds: u64,
    pub job: Option<String>,
    pub built: Option<String>,
    pub log: Option<String>,
    /// Which arm of a comparison, and which rep, the step belonged to.
    pub arm: Option<String>,
    pub rep: Option<u32>,
}

impl StepRecord {
    pub fn of(lock: &'static str, out: &crate::resource::Outcome) -> StepRecord {
        let t = out.trailer.as_ref();
        StepRecord {
            lock,
            status: out.status,
            seconds: out.seconds,
            job: t.map(|t| t.job.clone()),
            built: t.and_then(|t| t.built.clone()),
            log: t.and_then(|t| t.log.clone()),
            arm: None,
            rep: None,
        }
    }

    pub fn tagged(self, arm: Option<String>, rep: Option<u32>) -> StepRecord {
        StepRecord { arm, rep, ..self }
    }
}

/// One side of a comparison: what it was asked as, and what the machine built.
pub struct ArmRecord {
    pub name: String,
    /// The ref the machine fetched, None for the tree sent from here.
    pub fetched: Option<String>,
    pub revisions: Vec<(String, String)>,
    pub seeded: Option<String>,
}

/// Printed by a measured step before it runs, as `DIBS-STATE key=value ...`. Read from files
/// rather than tools, since it runs inside the exclusive lock: a governor other than
/// `performance`, or another driver, makes two runs of one recipe two histories.
pub fn stated(run: &str) -> String {
    format!(
        r#"echo "DIBS-STATE governor=$(cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null | sort -u | paste -sd+ -) kernel=$(uname -r) nvidia=$(grep -m1 -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' /proc/driver/nvidia/version 2>/dev/null | head -n 1)"
{run}"#
    )
}

/// The values a `DIBS-STATE` line carried, empty ones dropped.
pub fn state_of(report: &str) -> Vec<(String, String)> {
    report
        .lines()
        .filter_map(|l| l.strip_prefix("DIBS-STATE "))
        .last()
        .into_iter()
        .flat_map(|l| l.split_whitespace())
        .filter_map(|kv| kv.split_once('='))
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub struct Run {
    pub label: String,
    pub verb: &'static str,
    /// The repo's identity, which the label only starts with.
    pub repo: String,
    /// The worktree it came from, when that is not the repo's own checkout.
    pub variant: Option<String>,
    pub recipe: String,
    pub fingerprint: String,
    pub isolation: String,
    pub needs: Option<String>,
    /// Why this did not fit a recipe. Required for the ad-hoc verbs and absent otherwise, so
    /// the record itself distinguishes work that was specified from work that was improvised.
    pub reason: Option<String>,
    /// The procedure itself, not only its fingerprint. A recipe kept in local config is not
    /// recoverable by checking out a ref, so the record carries it: otherwise the fingerprint
    /// could say two runs differed without anyone being able to see how.
    pub procedure: Vec<(String, String)>,
    /// What the recipe's knobs were set to. Not in the label, deliberately: one label keeps one
    /// duration history, and a sweep that split it per value would predict none of its points.
    pub params: std::collections::BTreeMap<String, String>,
    pub backend: &'static str,
    /// Which card it ran on. Two devices under one label are two histories, exactly as two
    /// procedures under one label are: a number from one card cannot be compared with a
    /// number from another, and without this the record cannot say they differed.
    pub device: Option<String>,
    /// Which machine it ran on, when a pool was ranked rather than one machine named. Two
    /// machines' timings under one label are two distributions, and the record exists to say
    /// so rather than to let them be averaged.
    pub machine: Option<String>,
    /// Empty for a comparison, whose arms carry their own.
    pub revisions: Vec<(String, String)>,
    /// The sibling target directory a new tree's was copied from. A slow build with none is a
    /// build that started from nothing.
    pub seeded: Option<String>,
    /// A comparison's `@` as it was given, such as `main..local`.
    pub refs: Option<String>,
    pub arms: Vec<ArmRecord>,
    /// How many times what the recipe repeats ran, each step tagged with its rep.
    pub reps: u32,
    /// The batch this run was a step of, which is what ties the points and repetitions of one
    /// sweep together.
    pub batch: Option<String>,
    /// Measured with `--anyway` over a target another tree had built into.
    pub anyway: bool,
    /// Where the label's series started again, so older runs are not compared with this one.
    pub new_series: bool,
    /// The value each of the recipe's `fresh` variables had in this run.
    pub fresh: Vec<(String, String)>,
    /// What `stated` read on the machine when the measurement started.
    pub state: Vec<(String, String)>,
    pub steps: Vec<StepRecord>,
}

impl Run {
    /// A failed run stays in the file, so what went wrong can be looked up, and out of every
    /// listing and comparison that assumes a number came out of it.
    pub fn outcome(&self) -> &'static str {
        match self.steps.iter().all(|s| s.status == 0) {
            true => "ok",
            false => "failed",
        }
    }
}

impl Run {
    /// One line of JSON per run, appended. A format that survives being read by anything,
    /// including in five years by something that is not this program.
    pub fn to_json(&self, when: u64) -> String {
        let mut s = String::new();
        let _ = write!(s, "{{\"t\":{when},\"verb\":\"{}\"", self.verb);
        let _ = write!(s, ",\"label\":{}", q(&self.label));
        if !self.repo.is_empty() {
            let _ = write!(s, ",\"repo\":{}", q(&self.repo));
        }
        if let Some(v) = &self.variant {
            let _ = write!(s, ",\"variant\":{}", q(v));
        }
        let _ = write!(s, ",\"recipe\":{}", q(&self.recipe));
        let _ = write!(s, ",\"fingerprint\":{}", q(&self.fingerprint));
        let _ = write!(s, ",\"isolation\":{}", q(&self.isolation));
        let _ = write!(s, ",\"backend\":{}", q(self.backend));
        if let Some(m) = &self.machine {
            let _ = write!(s, ",\"machine\":{}", q(m));
        }
        if let Some(d) = &self.device {
            let _ = write!(s, ",\"device\":{}", q(d));
        }
        if let Some(n) = &self.needs {
            let _ = write!(s, ",\"needs\":{}", q(n));
        }
        if let Some(r) = &self.reason {
            let _ = write!(s, ",\"reason\":{}", q(r));
        }
        if let Some(r) = &self.seeded {
            let _ = write!(s, ",\"seeded\":{}", q(r));
        }
        if let Some(b) = &self.batch {
            let _ = write!(s, ",\"batch\":{}", q(b));
        }
        if let Some(r) = &self.refs {
            let _ = write!(s, ",\"refs\":{}", q(r));
        }
        if !self.arms.is_empty() {
            s.push_str(",\"arms\":[");
            for (i, a) in self.arms.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                let _ = write!(s, "{{\"name\":{}", q(&a.name));
                if let Some(f) = &a.fetched {
                    let _ = write!(s, ",\"fetched\":{}", q(f));
                }
                s.push_str(",\"revisions\":{");
                for (j, (name, sha)) in a.revisions.iter().enumerate() {
                    if j > 0 {
                        s.push(',');
                    }
                    let _ = write!(s, "{}:{}", q(name), q(sha));
                }
                s.push('}');
                if let Some(d) = &a.seeded {
                    let _ = write!(s, ",\"seeded\":{}", q(d));
                }
                s.push('}');
            }
            s.push(']');
        }
        if self.reps > 1 {
            let _ = write!(s, ",\"reps\":{}", self.reps);
        }
        if self.anyway {
            s.push_str(",\"anyway\":true");
        }
        if self.new_series {
            s.push_str(",\"new_series\":true");
        }
        if !self.fresh.is_empty() {
            s.push_str(",\"fresh\":{");
            for (i, (k, v)) in self.fresh.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                let _ = write!(s, "{}:{}", q(k), q(v));
            }
            s.push('}');
        }
        if !self.state.is_empty() {
            s.push_str(",\"state\":{");
            for (i, (k, v)) in self.state.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                let _ = write!(s, "{}:{}", q(k), q(v));
            }
            s.push('}');
        }
        if !self.params.is_empty() {
            s.push_str(",\"params\":{");
            for (i, (k, v)) in self.params.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                let _ = write!(s, "{}:{}", q(k), q(v));
            }
            s.push('}');
        }
        s.push_str(",\"procedure\":[");
        for (i, (lock, run)) in self.procedure.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "{{\"lock\":{},\"run\":{}}}", q(lock), q(run));
        }
        s.push_str("],\"revisions\":{");
        for (i, (name, sha)) in self.revisions.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "{}:{}", q(name), q(sha));
        }
        s.push_str("},\"steps\":[");
        for (i, st) in self.steps.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "{{\"lock\":\"{}\",\"status\":{},\"seconds\":{}", st.lock, st.status, st.seconds);
            if let Some(a) = &st.arm {
                let _ = write!(s, ",\"arm\":{}", q(a));
            }
            if let Some(r) = st.rep {
                let _ = write!(s, ",\"rep\":{r}");
            }
            for (key, value) in [("job", &st.job), ("built", &st.built), ("log", &st.log)] {
                if let Some(v) = value {
                    let _ = write!(s, ",\"{key}\":{}", q(v));
                }
            }
            s.push('}');
        }
        let _ = write!(s, "],\"outcome\":\"{}\"}}", self.outcome());
        s
    }
}

fn q(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_measured_step_says_what_state_the_machine_was_in_before_it_runs() {
        let out = std::process::Command::new("bash").arg("-c").arg(stated("echo RAN")).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(state_of(&text).iter().any(|(k, v)| k == "kernel" && !v.is_empty()), "{text}");
        assert!(text.find("DIBS-STATE").unwrap() < text.find("RAN").unwrap(), "{text}");
    }

    // A machine with no NVIDIA driver has no version for it, which is not a version called "".
    #[test]
    fn a_value_the_machine_did_not_have_is_left_out() {
        let state = state_of("noise\nDIBS-STATE governor=performance kernel=6.8.0 nvidia=\n");
        assert_eq!(state, [("governor".to_string(), "performance".to_string()), ("kernel".into(), "6.8.0".into())]);
    }

    #[test]
    fn a_record_carries_each_step_s_job_build_and_log_and_how_it_ended() {
        let step = |lock, status, job: &str, built: Option<&str>| StepRecord {
            lock,
            status,
            seconds: 1,
            job: Some(job.into()),
            built: built.map(str::to_string),
            log: Some(format!("m:/jobs/{job}/log")),
            arm: None,
            rep: None,
        };
        let run = Run {
            label: "r/bench/x".into(),
            verb: "bench",
            repo: "r".into(),
            variant: Some("topk-packed".into()),
            recipe: "x".into(),
            fingerprint: "f".into(),
            isolation: "machine".into(),
            needs: None,
            reason: None,
            procedure: vec![],
            params: Default::default(),
            backend: "dibs",
            device: None,
            machine: Some("m".into()),
            revisions: vec![],
            seeded: None,
            refs: None,
            arms: Vec::new(),
            reps: 1,
            batch: Some("20260918-1".into()),
            anyway: true,
            new_series: false,
            fresh: vec![("CUBECL_ENVIRONMENT".into(), "dibs-1".into())],
            state: vec![("governor".into(), "performance".into())],
            steps: vec![step("shared", 0, "1-1", Some("nothing")), step("exclusive", 3, "1-2", None)],
        };
        let v: serde_json::Value = serde_json::from_str(&run.to_json(1)).unwrap();
        assert_eq!(v["steps"][0]["built"], "nothing");
        assert_eq!(v["variant"], "topk-packed");
        assert_eq!(v["steps"][1]["job"], "1-2");
        assert_eq!(v["steps"][1]["log"], "m:/jobs/1-2/log");
        assert!(v["steps"][1].get("built").is_none());
        assert_eq!((&v["repo"], &v["batch"], &v["anyway"]), (&"r".into(), &"20260918-1".into(), &true.into()));
        assert_eq!(v["state"]["governor"], "performance");
        assert_eq!(v["fresh"]["CUBECL_ENVIRONMENT"], "dibs-1");
        assert_eq!(v["outcome"], "failed");
    }

    #[test]
    fn a_comparison_names_its_arms_and_tags_each_step() {
        let out = crate::resource::Outcome { status: 0, seconds: 4, trailer: None };
        let run = Run {
            label: "r/bench/x".into(),
            verb: "bench",
            repo: "r".into(),
            variant: None,
            recipe: "x".into(),
            fingerprint: "f".into(),
            isolation: "machine".into(),
            needs: None,
            reason: None,
            procedure: vec![],
            params: Default::default(),
            backend: "dibs",
            device: None,
            machine: None,
            revisions: vec![],
            seeded: None,
            refs: Some("main..local".into()),
            arms: vec![
                ArmRecord { name: "base".into(), fetched: Some("abc".into()), revisions: vec![("r".into(), "abc".into())], seeded: None },
                ArmRecord { name: "local".into(), fetched: None, revisions: vec![("r".into(), "local:d".into())], seeded: Some("r-local-1".into()) },
            ],
            reps: 2,
            batch: None,
            anyway: false,
            new_series: false,
            fresh: vec![],
            state: vec![],
            steps: vec![StepRecord::of("exclusive", &out).tagged(Some("local".into()), Some(2))],
        };
        let v: serde_json::Value = serde_json::from_str(&run.to_json(1)).unwrap();
        assert_eq!(v["refs"], "main..local");
        assert_eq!(v["arms"][0]["fetched"], "abc");
        assert!(v["arms"][1].get("fetched").is_none(), "the local tree was sent, not fetched");
        assert_eq!(v["arms"][1]["revisions"]["r"], "local:d");
        assert_eq!(v["arms"][1]["seeded"], "r-local-1");
        assert_eq!((&v["reps"], &v["steps"][0]["arm"], &v["steps"][0]["rep"]), (&2.into(), &"local".into(), &2.into()));
    }
}
