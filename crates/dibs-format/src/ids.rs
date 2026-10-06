use serde::{Deserialize, Serialize};
use std::{convert::Infallible, fmt, str::FromStr};

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl FromStr for $name {
            type Err = Infallible;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(s.to_string()))
            }
        }
    };
}

id!(
    /// The kind of work a duration is filed under, the same every time that work runs.
    Label
);
id!(
    /// A job's directory name on the machine, `dibs out <job>` finds it by.
    JobId
);
id!(
    /// One submission of `dibs batch`, which `dibs --kill <batch-id>` stops.
    BatchId
);
id!(
    /// A machine as the inventory names it.
    MachineName
);
id!(
    /// A card as the inventory names it, which `--device` takes.
    Alias
);

impl JobId {
    /// `text` as a job's id, only when it is one: digits and dashes, as a job's directory is
    /// named, so one read from a process's environment names no other path.
    pub fn checked(text: &str) -> Option<JobId> {
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
            .then(|| JobId::new(text))
    }
}

impl Label {
    /// The label as a machine files it: anything outside `[A-Za-z0-9._-]` becomes `_`.
    pub fn filed(&self) -> Label {
        let filed = self
            .0
            .chars()
            .map(|c| match c.is_ascii_alphanumeric() || "._-".contains(c) {
                true => c,
                false => '_',
            })
            .collect::<String>();
        Label(filed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_filed_with_only_the_characters_a_machine_keeps() {
        assert_eq!(
            Label::new("app/bench/held@cpu").filed(),
            Label::new("app_bench_held_cpu")
        );
        assert_eq!(Label::new("rec-a.b_c").filed(), Label::new("rec-a.b_c"));
    }
}
