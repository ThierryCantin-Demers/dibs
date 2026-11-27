use super::FileName;

id!(
    /// The kind of work a duration is filed under, the same every time that work runs.
    Label
);

impl Label {
    /// The label as a machine files it.
    pub fn to_filed(&self) -> Label {
        Label(FileName::of_text(&self.0).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_filed_with_only_the_characters_a_machine_keeps() {
        assert_eq!(
            Label::new("app/bench/held@cpu").to_filed(),
            Label::new("app_bench_held_cpu")
        );
    }

    #[test]
    fn a_label_a_machine_keeps_is_filed_as_it_is() {
        assert_eq!(Label::new("rec-a.b_c").to_filed(), Label::new("rec-a.b_c"));
    }
}
