use super::manifest::Recipe;
use sha2::{Digest, Sha256};

impl Recipe {
    /// Identifies the procedure a number was produced by. Recorded with every run, because a
    /// label alone is not provenance: one name can cover two different benchmarks at two refs,
    /// and comparing across that is the failure the history exists to prevent.
    ///
    /// Taken after the parameters are bound, so two values of one knob fingerprint apart: what
    /// ran is what has to be identified, not the template it came from.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        h.update(format!("{:?}", self.isolation).as_bytes());
        for s in &self.steps {
            h.update(format!("{:?}", s.lock).as_bytes());
            h.update(s.run.as_bytes());
            for (k, v) in &s.env {
                h.update(k.as_bytes());
                h.update(v.as_bytes());
            }
        }
        for f in &self.fresh {
            h.update(b"fresh");
            h.update(f.as_bytes());
        }
        format!("{:x}", h.finalize())[..16].to_string()
    }
}
