use std::{
    fs,
    path::{Path, PathBuf},
};

/// A path pattern as bash expands it with `globstar` and `nullglob`: `*`, `?` and `[...]` within
/// a name, `**` for any depth of directories, and hidden names matched only by a pattern that
/// starts with a dot.
pub struct Glob<'a> {
    pub pattern: &'a str,
}

impl Glob<'_> {
    /// The paths under `base` it matches, each as `base` joined with the matched part, sorted as
    /// bash sorts an expansion.
    pub fn under(&self, base: &Path) -> Vec<PathBuf> {
        let parts: Vec<&str> = self.pattern.split('/').filter(|p| !p.is_empty()).collect();
        let mut found = Vec::new();
        Glob::walk(base, PathBuf::new(), &parts, &mut found);
        found.sort();
        found.dedup();
        found
    }

    fn walk(base: &Path, at: PathBuf, parts: &[&str], found: &mut Vec<PathBuf>) {
        let Some((first, rest)) = parts.split_first() else {
            found.push(at);
            return;
        };
        if *first == "**" {
            Glob::walk(base, at.clone(), rest, found);
            for dir in Glob::entries(base, &at, "*") {
                if base.join(&dir).is_dir() && !base.join(&dir).is_symlink() {
                    Glob::walk(base, dir, parts, found);
                }
            }
            if rest.is_empty() {
                found.extend(Glob::entries(base, &at, "*"));
            }
            return;
        }
        if !first.contains(['*', '?', '[']) {
            let next = at.join(first);
            if base.join(&next).symlink_metadata().is_ok() {
                Glob::walk(base, next, rest, found);
            }
            return;
        }
        for entry in Glob::entries(base, &at, first) {
            Glob::walk(base, entry, rest, found);
        }
    }

    /// The names in `base/at` that `part` matches, as paths from `base`.
    fn entries(base: &Path, at: &Path, part: &str) -> Vec<PathBuf> {
        let Ok(listed) = fs::read_dir(base.join(at)) else {
            return Vec::new();
        };
        listed
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.') || part.starts_with('.'))
            .filter(|name| matches(part.as_bytes(), name.as_bytes()))
            .map(|name| at.join(name))
            .collect()
    }
}

/// Whether one name matches one pattern component.
fn matches(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| matches(rest, &name[skip..])),
        Some((b'?', rest)) => !name.is_empty() && matches(rest, &name[1..]),
        Some((b'[', rest)) => match (Class::read(rest), name.split_first()) {
            (Some(class), Some((c, name))) => class.holds(*c) && matches(class.after, name),
            (None, Some((b'[', name))) => matches(rest, name),
            _ => false,
        },
        Some((c, rest)) => name.first() == Some(c) && matches(rest, &name[1..]),
    }
}

/// A bracket expression: its ranges, whether it is negated, and the pattern after it.
struct Class<'a> {
    negated: bool,
    body: &'a [u8],
    after: &'a [u8],
}

impl<'a> Class<'a> {
    fn read(pattern: &'a [u8]) -> Option<Class<'a>> {
        let negated = matches!(pattern.first(), Some(b'!' | b'^'));
        let start = usize::from(negated);
        // A `]` first in the class is a member, not its end.
        let close = pattern
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, c)| **c == b']')
            .map(|(at, _)| at)?;
        Some(Class {
            negated,
            body: &pattern[start..close],
            after: &pattern[close + 1..],
        })
    }

    fn holds(&self, c: u8) -> bool {
        let mut held = false;
        let mut at = 0;
        while at < self.body.len() {
            let ranged = self.body.get(at + 1) == Some(&b'-') && at + 2 < self.body.len();
            match ranged {
                true => {
                    held |= (self.body[at]..=self.body[at + 2]).contains(&c);
                    at += 3;
                }
                false => {
                    held |= self.body[at] == c;
                    at += 1;
                }
            }
        }
        held != self.negated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_matches_as_bash_matches_it() {
        for (pattern, name, matched) in [
            ("*.json", "a.json", true),
            ("*.json", "a.jsonl", false),
            ("r?n", "run", true),
            ("[ab]x", "bx", true),
            ("[!ab]x", "bx", false),
            ("[a-c]1", "c1", true),
            ("[]]", "]", true),
            ("x[", "x[", true),
        ] {
            assert_eq!(
                matches(pattern.as_bytes(), name.as_bytes()),
                matched,
                "{pattern} {name}"
            );
        }
    }

    #[test]
    fn a_double_star_reaches_any_depth_and_a_hidden_name_only_when_asked() {
        let root = std::env::temp_dir().join(format!("dibs-glob-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["c/g/deep", "c/.hidden"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "c/e.json",
            "c/g/e.json",
            "c/g/deep/e.json",
            "c/.hidden/e.json",
            ".top.json",
        ] {
            fs::write(root.join(file), "").unwrap();
        }
        let found = |pattern: &str| -> Vec<String> {
            Glob { pattern }
                .under(&root)
                .iter()
                .map(|p| p.display().to_string())
                .collect()
        };
        assert_eq!(
            found("c/**/e.json"),
            ["c/e.json", "c/g/deep/e.json", "c/g/e.json"]
        );
        assert_eq!(found("*.json"), Vec::<String>::new());
        assert_eq!(found(".*.json"), [".top.json"]);
        assert_eq!(found("c/g/e.json"), ["c/g/e.json"]);
        let _ = fs::remove_dir_all(&root);
    }
}
