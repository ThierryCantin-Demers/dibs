//! `dibs hook ssh`: a Claude Code PreToolUse hook that refuses a shell command reaching a machine
//! in the inventory with ssh, scp, sftp or rsync, which would take no lock there.

use crate::{call::Rsh, inventory::Inventory};
use serde_json::Value;
use std::io::Read;

/// A PreToolUse hook that exits 2 stops the tool call and shows the agent its stderr.
const BLOCKS: i32 = 2;
/// What reaches a machine as a login or a copy.
const TOOLS: [Tool; 4] = [
    Tool {
        name: "ssh",
        reach: Reach::Login,
        valued: Valued::short("BbcDEeFIiJLlmOoPpQRSWw"),
    },
    Tool {
        name: "sftp",
        reach: Reach::Login,
        valued: Valued::short("BbcDFiJloPRSsX"),
    },
    Tool {
        name: "scp",
        reach: Reach::Copy,
        valued: Valued::short("cDFiJloPSX"),
    },
    Tool {
        name: "rsync",
        reach: Reach::Copy,
        valued: Valued {
            short: "eBfMT",
            long: &["--rsh"],
        },
    },
];
/// Words that run the command after them, as `sudo ssh box` runs ssh.
const WRAPPERS: [Wrapper; 10] = [
    Wrapper::of(
        "sudo",
        Valued {
            short: "ugCDprtTU",
            long: &["--user", "--group", "--chdir", "--prompt"],
        },
    ),
    Wrapper::of(
        "env",
        Valued {
            short: "uC",
            long: &["--unset", "--chdir"],
        },
    ),
    Wrapper::of("command", Valued::NONE),
    Wrapper::of("exec", Valued::short("a")),
    Wrapper::of("nohup", Valued::NONE),
    Wrapper::of("time", Valued::short("fo")),
    Wrapper::of(
        "nice",
        Valued {
            short: "n",
            long: &["--adjustment"],
        },
    ),
    Wrapper {
        name: "timeout",
        valued: Valued {
            short: "ks",
            long: &["--kill-after", "--signal"],
        },
        operands: 1,
    },
    Wrapper::of("xargs", Valued::short("aEdILnPs")),
    Wrapper::of("sshpass", Valued::short("fdpP")),
];
/// Shells whose `-c` runs the text after it.
const SHELLS: [&str; 4] = ["sh", "bash", "zsh", "dash"];

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
                entry.ssh.as_deref().map(|ssh| Destination(ssh).host()),
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
        Shell::commands(&Shell::without_heredocs(text))
            .into_iter()
            .find_map(|words| self.reaches(Words(&words)))
    }

    fn reaches(&self, words: Words) -> Option<Reached> {
        let words = words.unwrapped();
        let program = words.program()?;
        if SHELLS.contains(&program) {
            return words.shell_text().and_then(|text| self.reached(text));
        }
        let tool = TOOLS.iter().find(|t| t.name == program)?;
        if tool.name == "rsync" && words.through_dibs() {
            return None;
        }
        tool.destinations(words)
            .iter()
            .find_map(|d| self.machine(d.host()))
            .map(|machine| Reached {
                tool: tool.name.to_string(),
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

/// A tool that reaches another host, and how its words name that host.
struct Tool {
    name: &'static str,
    reach: Reach,
    valued: Valued,
}

/// Which of a tool's operands name the host it reaches.
#[derive(Clone, Copy)]
enum Reach {
    /// The first, as `ssh box ls` logs in to box.
    Login,
    /// Any written `host:path` or as a URL, as `scp f box:` copies to box; the rest are local.
    Copy,
}

/// The options a command takes a value with, as getopt reads them: a short one's value attached
/// or the next word, a long one's after `=` or the next word.
#[derive(Clone, Copy)]
struct Valued {
    short: &'static str,
    long: &'static [&'static str],
}

/// What one word is to a command that takes options.
#[derive(PartialEq, Eq)]
enum Role {
    Operand,
    Flag,
    /// An option whose value is the next word.
    FlagThenValue,
}

/// A word that runs the command after its own options and operands.
struct Wrapper {
    name: &'static str,
    valued: Valued,
    /// Operands of its own before the command, as `timeout`'s duration.
    operands: usize,
}

/// One simple command's words, quotes taken off.
#[derive(Clone, Copy)]
struct Words<'a>(&'a [String]);

/// A word naming the host a tool reaches: `box` of `user@box:path`, `box.local` of
/// `rsync://box.local/m`.
struct Destination<'a>(&'a str);

/// A heredoc a line opens: the word that ends it, and whether that word may be indented.
struct Heredoc {
    end: String,
    indented: bool,
}

impl Tool {
    fn destinations<'a>(&self, words: Words<'a>) -> Vec<Destination<'a>> {
        let mut operands = words
            .operands(1, self.valued)
            .into_iter()
            .map(|at| words.0[at].as_str());
        match self.reach {
            Reach::Login => operands.next().map(Destination).into_iter().collect(),
            Reach::Copy => operands.filter_map(Destination::copied).collect(),
        }
    }
}

impl Valued {
    const NONE: Valued = Valued::short("");

    const fn short(short: &'static str) -> Valued {
        Valued { short, long: &[] }
    }

    fn role(&self, word: &str) -> Role {
        if word == "-" || !word.starts_with('-') {
            return Role::Operand;
        }
        if word.starts_with("--") {
            return match !word.contains('=') && self.long.contains(&word) {
                true => Role::FlagThenValue,
                false => Role::Flag,
            };
        }
        let cluster = &word[1..];
        match cluster
            .char_indices()
            .find(|(_, c)| self.short.contains(*c))
        {
            Some((at, c)) if at + c.len_utf8() == cluster.len() => Role::FlagThenValue,
            _ => Role::Flag,
        }
    }
}

impl Wrapper {
    const fn of(name: &'static str, valued: Valued) -> Wrapper {
        Wrapper {
            name,
            valued,
            operands: 0,
        }
    }

    fn named(name: &str) -> Option<&'static Wrapper> {
        WRAPPERS.iter().find(|w| w.name == name)
    }
}

impl<'a> Words<'a> {
    /// The program, by its file name.
    fn program(&self) -> Option<&'a str> {
        self.0.first().map(|w| w.rsplit('/').next().unwrap_or(w))
    }

    /// The command once the assignments and wrappers in front of it are gone.
    fn unwrapped(self) -> Words<'a> {
        let mut at = self.assignments(0);
        while let Some(wrapper) = Words(&self.0[at..]).program().and_then(Wrapper::named) {
            let command = self.operands(at + 1, wrapper.valued).first().copied();
            at = command.map_or(self.0.len(), |command| {
                (self.assignments(command) + wrapper.operands).min(self.0.len())
            });
        }
        Words(&self.0[at..])
    }

    /// Where the assignments from `from` on end.
    fn assignments(&self, from: usize) -> usize {
        let assignment = |w: &String| {
            w.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        };
        from + self.0[from..].iter().take_while(|w| assignment(w)).count()
    }

    /// Where its operands are from `from` on, every option and the value one takes skipped.
    fn operands(&self, from: usize, valued: Valued) -> Vec<usize> {
        let mut found = Vec::new();
        let mut at = from;
        while let Some(word) = self.0.get(at) {
            at += 1;
            if word == "--" {
                found.extend(at..self.0.len());
                break;
            }
            match valued.role(word) {
                Role::Operand => found.push(at - 1),
                Role::FlagThenValue => at += 1,
                Role::Flag => {}
            }
        }
        found
    }

    /// An rsync whose transport is dibs's own, which takes the lock.
    fn through_dibs(&self) -> bool {
        self.0.iter().enumerate().any(|(i, w)| {
            let value = match w.as_str() {
                "-e" | "--rsh" => self.0.get(i + 1).map(String::as_str),
                _ => w.strip_prefix("--rsh=").or_else(|| w.strip_prefix("-e")),
            };
            value.is_some_and(|v| v.contains(Rsh::WORD))
        })
    }

    /// The text a shell's `-c` runs.
    fn shell_text(&self) -> Option<&'a str> {
        let flag = self.0.iter().position(|w| {
            w.len() > 1 && w.starts_with('-') && !w.starts_with("--") && w[1..].contains('c')
        })?;
        self.0[flag + 1..]
            .iter()
            .find(|w| !w.starts_with(['-', '+']))
            .map(String::as_str)
    }
}

impl<'a> Destination<'a> {
    /// A copy's operand when it names a host; a word with no colon, or a slash before its first,
    /// is a local path, as scp and rsync read it.
    fn copied(word: &'a str) -> Option<Destination<'a>> {
        let remote = word.contains("://")
            || word
                .split_once(':')
                .is_some_and(|(before, _)| !before.is_empty() && !before.contains('/'));
        remote.then_some(Destination(word))
    }

    fn host(&self) -> &'a str {
        let word = self.0;
        let word = word.split_once("://").map_or(word, |(_, rest)| rest);
        let word = word.rsplit_once('@').map_or(word, |(_, host)| host);
        let end = word.find([':', '/']).unwrap_or(word.len());
        &word[..end]
    }
}

impl Heredoc {
    /// The heredoc a line opens, its word read as the shell reads one.
    fn opened_by(line: &str) -> Option<Heredoc> {
        let mut rest = line;
        while let Some(at) = rest.find("<<") {
            let after = &rest[at + 2..];
            if let Some(string) = after.strip_prefix('<') {
                rest = string;
                continue;
            }
            let indented = after.starts_with('-');
            let end: String = after
                .strip_prefix('-')
                .unwrap_or(after)
                .trim_start()
                .chars()
                .take_while(|c| !matches!(c, ' ' | '\t' | '<' | '>' | ';' | '|' | '&'))
                .filter(|c| !matches!(c, '\'' | '"' | '\\'))
                .collect();
            if !end.is_empty() {
                return Some(Heredoc { end, indented });
            }
            rest = after;
        }
        None
    }

    fn ends_at(&self, line: &str) -> bool {
        match self.indented {
            true => line.trim_start() == self.end,
            false => line == self.end,
        }
    }
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
    /// The text with the bodies of its heredocs left out, so what a file says is never read as a
    /// command.
    fn without_heredocs(text: &str) -> String {
        let mut out = String::new();
        let mut open: Option<Heredoc> = None;
        for line in text.lines() {
            if let Some(heredoc) = &open {
                if heredoc.ends_at(line) {
                    open = None;
                }
                continue;
            }
            out.push_str(line);
            out.push('\n');
            open = Heredoc::opened_by(line);
        }
        out
    }

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
            "nice -n 10 ssh box-a",
            "sudo -u root ssh box-a",
            "timeout -k 5 30 ssh box-a",
            "bash -c 'ssh box-a ls'",
            "sudo sh -ec 'ls; scp f box-a:'",
            "xargs ssh box-a",
            "sshpass -p x ssh box-a",
            "cat <<END-OF\nhi\nEND-OF\nssh box-a",
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
            "bash -c 'echo ssh box-a'",
        ] {
            assert_eq!(reaches(command), None, "{command}");
        }
    }

    #[test]
    fn a_local_path_named_like_a_machine_passes() {
        for command in [
            "rsync -a box-a/ /backup/",
            "scp box-a.txt other:/tmp/",
            "rsync -a box-a.toml backup:",
            "scp ./box-a:notes other:",
            "ssh -i box-a github.com",
            "ssh -l box-a github.com",
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
