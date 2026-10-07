use super::parse::Step;

pub fn plan(steps: &[Step], machines: &[String]) -> String {
    let w = steps.iter().map(|s| s.name.len()).max().unwrap_or(4).max(4);
    let m = machines.iter().map(String::len).max().unwrap_or(7).max(7);
    let mut s = String::new();
    for (i, st) in steps.iter().enumerate() {
        let after = if st.after.is_empty() {
            "-".to_string()
        } else {
            st.after.join(",")
        };
        s.push_str(&format!(
            "  {:w$}  {:m$}  {:6}  after {}{}\n",
            st.name,
            machines[i],
            st.lock,
            after,
            if st.cont { ", cont" } else { "" }
        ));
    }
    s
}
