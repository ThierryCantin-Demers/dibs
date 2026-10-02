use std::fmt::{self, Write as _};

/// The command a call runs: the words after the flags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Command(pub Vec<String>);

impl Command {
    pub fn words(&self) -> &[String] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// One word is a shell string, as ssh takes it. Several are each quoted as bash's
    /// `printf '%q '` does, trailing space included, so a word with spaces stays one word.
    pub fn shell_string(&self) -> String {
        match self.0.as_slice() {
            [one] => one.clone(),
            words => words.iter().fold(String::new(), |mut line, word| {
                let _ = write!(line, "{} ", BashQuoted(word));
                line
            }),
        }
    }
}

/// A word quoted as bash's `printf %q` quotes it.
pub struct BashQuoted<'a>(pub &'a str);

impl BashQuoted<'_> {
    /// Characters `printf %q` puts a backslash before.
    const SPECIAL: &'static str = " \t\n!\"$&'()*,;<>?[\\]^`{|}";

    /// A word with an unprintable character is written as `$'...'` instead.
    fn needs_ansi_c(&self) -> bool {
        self.0.chars().any(|c| c.is_control())
    }

    fn ansi_c(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("$'")?;
        for c in self.0.chars() {
            match c {
                '\x1b' => f.write_str("\\E")?,
                '\x07' => f.write_str("\\a")?,
                '\x0b' => f.write_str("\\v")?,
                '\x08' => f.write_str("\\b")?,
                '\x0c' => f.write_str("\\f")?,
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                '\\' | '\'' => write!(f, "\\{c}")?,
                c if c.is_control() => {
                    let mut bytes = [0; 4];
                    for byte in c.encode_utf8(&mut bytes).bytes() {
                        write!(f, "\\{byte:03o}")?;
                    }
                }
                c => f.write_char(c)?,
            }
        }
        f.write_str("'")
    }
}

impl fmt::Display for BashQuoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("''");
        }
        if self.needs_ansi_c() {
            return self.ansi_c(f);
        }
        let mut previous = None;
        for (i, c) in self.0.chars().enumerate() {
            let tilde_starts = matches!(previous, None | Some(':' | '='));
            let escaped = BashQuoted::SPECIAL.contains(c)
                || (c == '#' && i == 0)
                || (c == '~' && tilde_starts);
            if escaped {
                f.write_char('\\')?;
            }
            f.write_char(c)?;
            previous = Some(c);
        }
        Ok(())
    }
}

/// A word as a POSIX shell reads it back: bare when that is safe, single-quoted otherwise.
pub struct ShellWord<'a>(pub &'a str);

impl fmt::Display for ShellWord<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bare = !self.0.is_empty()
            && self
                .0
                .chars()
                .all(|c| c.is_alphanumeric() || "/._-@".contains(c));
        match bare {
            true => f.write_str(self.0),
            false => write!(f, "'{}'", self.0.replace('\'', r"'\''")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What bash itself prints, to hold the quoting to it rather than to a reading of its source.
    fn bash_q(word: &str) -> String {
        let out = std::process::Command::new("bash")
            .args(["-c", "printf %q \"$1\"", "_", word])
            .env("LC_ALL", "C.UTF-8")
            .output()
            .expect("bash runs");
        String::from_utf8(out.stdout).expect("utf-8")
    }

    #[test]
    fn a_word_is_quoted_as_bash_quotes_it() {
        for word in [
            "plain",
            "with space",
            "",
            "it's",
            "a\"b",
            "$HOME/x",
            "a;b|c&d",
            "(x)<y>",
            "*?[a]",
            "{a,b}",
            "^caret`tick`",
            "back\\slash",
            "#first",
            "not#first",
            "~home",
            "a=~b",
            "a:~b",
            "x~y",
            "tab\there",
            "new\nline",
            "esc\x1b[0m",
            "bell\x07",
            "del\x7f",
            "café",
            "naïve space",
            "%+-./:=@_",
        ] {
            assert_eq!(BashQuoted(word).to_string(), bash_q(word), "{word:?}");
        }
    }

    #[test]
    fn one_word_is_a_shell_string_and_several_are_each_quoted() {
        let one = Command(vec!["echo hi; ls".into()]);
        assert_eq!(one.shell_string(), "echo hi; ls");
        let several = Command(vec!["seq".into(), "1".into(), "a b".into()]);
        assert_eq!(several.shell_string(), "seq 1 a\\ b ");
        assert_eq!(Command::default().shell_string(), "");
    }

    #[test]
    fn a_shell_word_is_bare_only_when_that_is_safe() {
        assert_eq!(ShellWord("app@main").to_string(), "app@main");
        assert_eq!(ShellWord("a b").to_string(), "'a b'");
        assert_eq!(ShellWord("it's").to_string(), r"'it'\''s'");
        assert_eq!(ShellWord("").to_string(), "''");
    }
}
