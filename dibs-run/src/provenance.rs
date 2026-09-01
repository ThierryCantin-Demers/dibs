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
}

pub struct Run {
    pub label: String,
    pub verb: &'static str,
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
    pub backend: &'static str,
    /// Which card it ran on. Two devices under one label are two histories, exactly as two
    /// procedures under one label are: a number from one card cannot be compared with a
    /// number from another, and without this the record cannot say they differed.
    pub device: Option<String>,
    /// Which machine it ran on, when a pool was ranked rather than one machine named. Two
    /// machines' timings under one label are two distributions, and the record exists to say
    /// so rather than to let them be averaged.
    pub machine: Option<String>,
    pub revisions: Vec<(String, String)>,
    pub steps: Vec<StepRecord>,
}

impl Run {
    /// One line of JSON per run, appended. A format that survives being read by anything,
    /// including in five years by something that is not this program.
    pub fn to_json(&self, when: u64) -> String {
        let mut s = String::new();
        let _ = write!(s, "{{\"t\":{when},\"verb\":\"{}\"", self.verb);
        let _ = write!(s, ",\"label\":{}", q(&self.label));
        let _ = write!(s, ",\"recipe\":{}", q(&self.recipe));
        let _ = write!(s, ",\"fingerprint\":{}", q(&self.fingerprint));
        let _ = write!(s, ",\"isolation\":{}", q(&self.isolation));
        let _ = write!(s, ",\"backend\":{}", q(self.backend));
        if let Some(m) = &self.machine {
            let _ = write!(s, ",\"machine\":{}", q(m));
        }
        if let Some(n) = &self.needs {
            let _ = write!(s, ",\"needs\":{}", q(n));
        }
        if let Some(r) = &self.reason {
            let _ = write!(s, ",\"reason\":{}", q(r));
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
            let _ = write!(
                s,
                "{{\"lock\":\"{}\",\"status\":{},\"seconds\":{}}}",
                st.lock, st.status, st.seconds
            );
        }
        s.push_str("]}");
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
