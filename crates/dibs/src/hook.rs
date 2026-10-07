//! `dibs hook ssh`: a Claude Code PreToolUse hook that refuses a shell command reaching a machine
//! in the inventory with ssh, scp, sftp or rsync, which would take no lock there.

use crate::{call::Rsh, inventory::Inventory};
use serde_json::Value;
use std::io::Read;

/// A PreToolUse hook that exits 2 stops the tool call and shows the agent its stderr.
const BLOCKS: i32 = 2;
/// What reaches a machine as a login or a copy.
const TOOLS: [&str; 4] = ["ssh", "scp", "sftp", "rsync"];
/// Words that run the command after them, as `sudo ssh box` runs ssh.
const WRAPPERS: [&str; 8] = [
    "sudo", "env", "command", "exec", "nohup", "time", "nice", "timeout",
];

/// The names the inventory's machines are reached by, each with the machine it names.
pub struct SshHook {
    names: Vec<Known>,
}

struct Known {
    name: String,
    machine: String,
}

/// A command that reaches a machine around its lock.
#[derive(Debug, PartialEq, Eq)]
pub struct Reached {
    pub tool: String,
    pub machine: String,
}

impl SshHook {
    pub fn of(inventory: &Inventory) -> SshHook {
        let mut names = Vec::new();
        for machine in inventory.names() {
            let Some(entry) = inventory.machine(machine.as_str()) else {
                continue;
            };
            let said = [
                Some(machine.as_str()),
                entry.ssh.as_deref().map(host),
                entry.hostname.as_deref(),
            ];
            for name in said.into_iter().flatten().filter(|n| !n.is_empty()) {
                names.push(Known {
                    name: name.to_ascii_lowercase(),
                    machine: machine.to_string(),
                });
            }
        }
        SshHook { names }
    }

    /// Reads the tool call on stdin, and refuses it on stderr when it reaches a machine.
    pub fn serve() -> i32 {
        let mut input = String::new();
        let _ = std::io::stdin().read_to_string(&mut input);
        let command = serde_json::from_str::<Value>(&input)
            .ok()
            .and_then(|call| {
                call.pointer("/tool_input/command")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        // A hook that cannot read the inventory knows no machine, and lets the command through.
        let Ok(Some(inventory)) = Inventory::here() else {
            return 0;
        };
        match SshHook::of(&inventory).reached(&command) {
            Some(reached) => {
                eprint!("{}", reached.refusal());
                BLOCKS
            }
            None => 0,
        }
    }

    /// The first machine a command reaches with one of the tools, outside dibs's own transport.
    pub fn reached(&self, text: &str) -> Option<Reached> {
        Shell::commands(&without_heredocs(text))
            .into_iter()
            .find_map(|words| self.reaches(&words))
    }

    fn reaches(&self, words: &[String]) -> Option<Reached> {
        let words = unwrapped(words);
        let tool = words.first().map(|w| w.rsplit('/').next().unwrap_or(w))?;
        if !TOOLS.contains(&tool) || (tool == "rsync" && through_dibs(words)) {
            return None;
        }
        words[1..]
            .iter()
            .filter(|w| !w.starts_with('-'))
            .find_map(|w| self.machine(host(w)))
            .map(|machine| Reached {
                tool: tool.to_string(),
                machine,
            })
    }

    fn machine(&self, host: &str) -> Option<String> {
        let host = host.to_ascii_lowercase();
        let names = |a: &str, b: &str| a.strip_prefix(b).is_some_and(|rest| rest.starts_with('.'));
        self.names
            .iter()
            .find(|k| k.name == host || names(&host, &k.name) || names(&k.name, &host))
            .map(|k| k.machine.clone())
    }
}

impl Reached {
    fn refusal(&self) -> String {
        let Reached { tool, machine } = self;
        let instead = [
            (
                format!("dibs --on {machine} <command>"),
                "builds, tests and a look around, beside other shared work",
            ),
            (
                format!("dibs --on {machine} --bench <command>"),
                "a measurement, with nothing else beside it",
            ),
            (
                format!("dibs --on {machine} --sync -a ./tree :~/tree"),
                "rsync either way under the shared lock",
            ),
            (
                format!("dibs --on {machine} --peek <command>"),
                "a look that takes no lock: ps, nvidia-smi, ls, tail",
            ),
            (
                "dibs status".to_string(),
                "who holds each machine and who waits",
            ),
        ];
        let width = instead
            .iter()
            .map(|(call, _)| call.len())
            .max()
            .unwrap_or(0);
        let mut said = format!(
            "dibs: {tool} reaches {machine} around its lock. Nothing was run.\n  \
             Several agents use it at once, and a command that takes no lock spoils whoever is\n  \
             measuring there. A copy is no better: it competes with a measurement for memory\n  \
             bandwidth and writeback. Through dibs instead:\n"
        );
        for (call, what) in instead {
            said.push_str(&format!("    {call:<width$}  {what}\n"));
        }
        said
    }
}

/// The host a word names: `box` of `user@box:path`, `box.local` of `rsync://box.local/m`.
fn host(word: &str) -> &str {
    let word = word.split_once("://").map_or(word, |(_, rest)| rest);
    let word = word.rsplit_once('@').map_or(word, |(_, host)| host);
    let end = word.find([':', '/']).unwrap_or(word.len());
    &word[..end]
}

/// An rsync whose transport is dibs's own, which takes the lock.
fn through_dibs(words: &[String]) -> bool {
    words.iter().enumerate().any(|(i, w)| {
        let value = match w.as_str() {
            "-e" | "--rsh" => words.get(i + 1).map(String::as_str),
            _ => w.strip_prefix("--rsh=").or_else(|| w.strip_prefix("-e")),
        };
        value.is_some_and(|v| v.contains(Rsh::WORD))
    })
}

/// The command once the assignments and wrappers in front of it are gone.
fn unwrapped(words: &[String]) -> &[String] {
    let assignment = |w: &str| {
        w.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    };
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let name = word.rsplit('/').next().unwrap_or(word);
        if assignment(word) {
            at += 1;
        } else if WRAPPERS.contains(&name) {
            at += 1;
            while words
                .get(at)
                .is_some_and(|w| w.starts_with('-') || assignment(w))
            {
                at += 1;
            }
            if name == "timeout"
                && words
                    .get(at)
                    .is_some_and(|w| w.starts_with(|c: char| c.is_ascii_digit()))
            {
                at += 1;
            }
        } else {
            break;
        }
    }
    &words[at..]
}

/// The text with the bodies of its heredocs left out, so what a file says is never read as a
/// command.
fn without_heredocs(text: &str) -> String {
    let mut out = String::new();
    let mut until: Option<(String, bool)> = None;
    for line in text.lines() {
        if let Some((end, dashed)) = &until {
            let line = if *dashed { line.trim_start() } else { line };
            if line == end {
                until = None;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        until = heredoc(line);
    }
    out
}

/// The word that ends a heredoc a line starts, and whether its lines may be indented.
fn heredoc(line: &str) -> Option<(String, bool)> {
    let mut rest = line;
    while let Some(at) = rest.find("<<") {
        let after = &rest[at + 2..];
        if let Some(string) = after.strip_prefix('<') {
            rest = string;
            continue;
        }
        let dashed = after.starts_with('-');
        let word: String = after
            .trim_start_matches('-')
            .trim_start()
            .trim_start_matches(['\'', '"'])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !word.is_empty() {
            return Some((word, dashed));
        }
        rest = after;
    }
    None
}

/// A shell text split into its simple commands, each into its words, quotes taken off.
#[derive(Default)]
struct Shell {
    commands: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    in_word: bool,
}

impl Shell {
    fn commands(text: &str) -> Vec<Vec<String>> {
        let mut shell = Shell::default();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\'' => {
                    shell.in_word = true;
                    shell.word.extend(chars.by_ref().take_while(|x| *x != '\''));
                }
                '"' => {
                    shell.in_word = true;
                    while let Some(x) = chars.next() {
                        match x {
                            '"' => break,
                            '\\' => shell.word.extend(chars.next()),
                            '$' if chars.peek() == Some(&'(') => {
                                chars.next();
                                let inner = Shell::closing(&mut chars, '(', ')');
                                shell.commands.extend(Shell::commands(&inner));
                            }
                            '`' => {
                                let inner: String =
                                    chars.by_ref().take_while(|y| *y != '`').collect();
                                shell.commands.extend(Shell::commands(&inner));
                            }
                            x => shell.word.push(x),
                        }
                    }
                }
                '\\' => match chars.next() {
                    Some('\n') | None => {}
                    Some(x) => {
                        shell.in_word = true;
                        shell.word.push(x);
                    }
                },
                ';' | '&' | '|' | '\n' | '(' | ')' | '`' => shell.end_command(),
                '$' if chars.peek() == Some(&'(') => {
                    chars.next();
                    shell.end_command();
                }
                c if c.is_whitespace() => shell.end_word(),
                c => {
                    shell.in_word = true;
                    shell.word.push(c);
                }
            }
        }
        shell.end_command();
        shell.commands
    }

    /// The text up to the bracket that closes one already opened.
    fn closing(chars: &mut impl Iterator<Item = char>, open: char, close: char) -> String {
        let mut depth = 1;
        let mut inner = String::new();
        for c in chars {
            depth += i32::from(c == open) - i32::from(c == close);
            if depth == 0 {
                break;
            }
            inner.push(c);
        }
        inner
    }

    fn end_word(&mut self) {
        if self.in_word {
            self.words.push(std::mem::take(&mut self.word));
            self.in_word = false;
        }
    }

    fn end_command(&mut self) {
        self.end_word();
        if !self.words.is_empty() {
            self.commands.push(std::mem::take(&mut self.words));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVENTORY: &str = r#"
[machine.box-a]
ssh      = "dibs@box-a"
hostname = "box-a"

[machine.box-b]
ssh      = "box-b.local"
hostname = "workstation-b"
"#;

    fn hook() -> SshHook {
        SshHook::of(&Inventory::parse(INVENTORY).unwrap())
    }

    fn reaches(command: &str) -> Option<String> {
        hook().reached(command).map(|r| r.machine)
    }

    #[test]
    fn every_name_a_machine_goes_by_is_refused() {
        for command in [
            "ssh box-a",
            "ssh dibs@box-a 'nvidia-smi'",
            "ssh -p 22 -i ~/.ssh/id box-a.example.ts.net uptime",
            "scp file box-a:/tmp/",
            "rsync -a ./tree dibs@box-a:~/tree",
            "rsync -a rsync://box-a/module/ .",
            "sftp box-a",
            "/usr/bin/ssh BOX-A",
        ] {
            assert_eq!(reaches(command).as_deref(), Some("box-a"), "{command}");
        }
        for command in ["ssh box-b", "ssh box-b.local", "scp x workstation-b:y"] {
            assert_eq!(reaches(command).as_deref(), Some("box-b"), "{command}");
        }
    }

    #[test]
    fn a_command_hidden_behind_another_is_found() {
        for command in [
            "cd /x && ssh box-a ls",
            "true; scp a box-a:b",
            "echo hi | ssh box-a cat",
            "x=$(ssh box-a hostname)",
            "echo \"$(ssh box-a hostname)\"",
            "echo `ssh box-a hostname`",
            "(ssh box-a)",
            "FOO=1 sudo -E timeout 30 ssh box-a",
            "env A=b ssh box-a",
            "ssh \\\n  box-a",
        ] {
            assert_eq!(reaches(command).as_deref(), Some("box-a"), "{command}");
        }
    }

    #[test]
    fn what_reaches_no_machine_or_goes_through_dibs_passes() {
        for command in [
            "ssh github.com",
            "ssh box-ab",
            "ssh box-a-other",
            "git push origin box-a",
            "dibs --on box-a 'ssh box-b ls'",
            "dibs --sync -a ./tree :~/tree",
            "rsync -e 'dibs __rsh' -a ./tree box-a:~/tree",
            "rsync --rsh='dibs __rsh' -a ./tree box-a:~/tree",
            "echo 'ssh box-a'",
            "grep -r 'scp box-a' .",
            "cat <<EOF\nssh box-a\nEOF\nls",
            "cat <<-'END'\n\tssh box-a\n\tEND\n",
            "cat <<< 'ssh box-a'",
        ] {
            assert_eq!(reaches(command), None, "{command}");
        }
    }

    #[test]
    fn a_heredoc_body_is_skipped_but_what_follows_it_is_read() {
        assert_eq!(
            reaches("cat <<EOF > notes\nssh box-b\nEOF\nssh box-a").as_deref(),
            Some("box-a")
        );
    }

    #[test]
    fn the_refusal_names_the_tool_and_the_machine() {
        let refused = hook().reached("scp f box-b.local:").unwrap();
        assert_eq!(
            refused,
            Reached {
                tool: "scp".into(),
                machine: "box-b".into()
            }
        );
        assert!(
            refused
                .refusal()
                .starts_with("dibs: scp reaches box-b around its lock. Nothing was run.\n")
        );
    }
}
