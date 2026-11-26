use dibs_format::Exit;

/// What `--help` prints.
pub struct Help;

/// A subcommand's help: the entries of the whole help that are its own, then the options it
/// takes, each named by how its line in the whole help begins.
struct Topic {
    words: &'static [&'static str],
    entries: &'static [&'static str],
}

impl Help {
    const INTERFACE: &'static str = include_str!("help.txt");
    const SHORT: &'static str = include_str!("help-short.txt");

    /// The exits a caller acts on, one row per line of the help.
    const EXITS: [&'static [Exit]; 4] = [
        &[Exit::Unreachable, Exit::NoRoom, Exit::NoLock],
        &[Exit::NoRunner, Exit::Busy, Exit::Cancelled],
        &[Exit::ServiceFailed, Exit::Overran],
        &[Exit::TargetRebuilt],
    ];

    const TOPICS: &'static [Topic] = &[
        Topic {
            words: &["build", "test", "bench"],
            entries: &[
                "dibs <verb>",
                "<verb>",
                "<repo>",
                "<ref>",
                "--root",
                "--device     the card",
                "--<name>",
                "--sweep",
                "--reps",
                "--pin",
                "--artifacts",
                "--anyway",
                "--image",
                "--new-series",
                "--dry-run",
                "--on <machine>",
                "--max <seconds>",
            ],
        },
        Topic {
            words: &["list"],
            entries: &["dibs list", "<repo>", "--root"],
        },
        Topic {
            words: &["runs"],
            entries: &["dibs runs"],
        },
        Topic {
            words: &["measurement"],
            entries: &["dibs measurement"],
        },
        Topic {
            words: &["shell"],
            entries: &[
                "dibs shell",
                "<repo>",
                "<ref>",
                "--root",
                "--reason",
                "--bench      shell only",
                "--preemptible  shell only",
                "--label      shell only",
                "--device     the card",
                "--pin",
                "--dry-run",
                "--on <machine>",
                "--max <seconds>",
            ],
        },
        Topic {
            words: &["raw"],
            entries: &["dibs raw", "--reason", "--on <machine>", "--max <seconds>"],
        },
        Topic {
            words: &["with"],
            entries: &[
                "dibs with",
                "<repo>",
                "<ref>",
                "--root",
                "--device     the card",
                "--pin",
                "--dry-run",
                "--on <machine>",
            ],
        },
        Topic {
            words: &["gaps"],
            entries: &["dibs gaps"],
        },
        Topic {
            words: &["machines"],
            entries: &["dibs machines [<machine>]"],
        },
        Topic {
            words: &["batch"],
            entries: &[
                "dibs batch <file",
                "dibs batch --detach",
                "dibs batch --follow",
                "--dry-run",
                "--verbose",
            ],
        },
        Topic {
            words: &["run"],
            entries: &[
                "dibs run <command>",
                "dibs run --bench",
                "dibs --hold",
                "--with <name>",
                "--port <name>",
                "--ready tcp",
                "--ready '",
                "--device <alias>",
                "--stream",
                "--new-series",
                "--preemptible                a shared",
                "--on <machine>",
                "--wait <seconds>",
                "--max <seconds>",
                "--check-in <time>",
                "--label <name>",
            ],
        },
        Topic {
            words: &["status"],
            entries: &[
                "dibs --status",
                "dibs --watch",
                "--json",
                "--metrics <machines|all>",
                "--all",
                "--on <machine>",
                "-v ",
            ],
        },
        Topic {
            words: &["out"],
            entries: &["dibs --out", "--on <machine>"],
        },
        Topic {
            words: &["follow"],
            entries: &["dibs --follow", "--on <machine>"],
        },
        Topic {
            words: &["fetch"],
            entries: &["dibs --fetch", "--on <machine>"],
        },
        Topic {
            words: &["gc"],
            entries: &["dibs --gc", "--on <machine>"],
        },
        Topic {
            words: &["friction"],
            entries: &["dibs --friction '<line>'"],
        },
        Topic {
            words: &["completions"],
            entries: &["dibs completions"],
        },
        Topic {
            words: &["hook"],
            entries: &["dibs hook ssh", "dibs hook urgent"],
        },
    ];

    /// `dibs --help --all`: the interface, then what each exit means.
    pub fn text() -> String {
        Help::with_exits(Help::INTERFACE)
    }

    /// `dibs --help`: a line for each command, then what each exit means.
    pub fn short() -> String {
        Help::with_exits(Help::SHORT)
    }

    fn with_exits(interface: &str) -> String {
        let mut text = interface.to_string();
        for (row, exits) in Help::EXITS.iter().enumerate() {
            let lead = match row {
                0 => "  exit ",
                _ => "       ",
            };
            let cells: Vec<String> = exits
                .iter()
                .map(|e| format!("{} {}", e.code(), e.meaning()))
                .collect();
            text.push_str(&format!("{lead}{}\n", cells.join("    ")));
        }
        text.push('\n');
        text
    }

    /// What `--help` prints: the short help alone, the whole help with `all`, and after a
    /// subcommand that subcommand's part of the whole help, or the whole help for a word that has
    /// none.
    pub fn of(subcommand: Option<&str>) -> String {
        let Some(word) = subcommand else {
            return Help::short();
        };
        let Some(topic) = Help::TOPICS.iter().find(|t| t.words.contains(&word)) else {
            return Help::text();
        };
        let mut text: String = topic
            .entries
            .iter()
            .filter_map(|e| Help::entry(e))
            .collect();
        text.push_str(
            "\ndibs --help --all shows every command and flag, and what each exit means.\n",
        );
        text
    }

    /// The entry of the whole help whose line begins with `begins`, with the lines that continue
    /// it, which are indented further.
    fn entry(begins: &str) -> Option<String> {
        let lines: Vec<&str> = Help::INTERFACE.lines().collect();
        let first = lines.iter().position(|line| {
            line.strip_prefix("  ")
                .is_some_and(|rest| rest.starts_with(begins))
        })?;
        let more = lines[first + 1..]
            .iter()
            .take_while(|line| line.starts_with("   ") && !line.trim().is_empty())
            .count();
        Some(
            lines[first..=first + more]
                .iter()
                .map(|line| format!("{line}\n"))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Help;

    #[test]
    fn the_short_help_names_every_command_of_the_whole_help() {
        let word = |w: &str| w.trim_matches(|c: char| ",;|[]'".contains(c)).to_string();
        let short: Vec<String> = Help::SHORT.split_whitespace().map(word).collect();
        for line in Help::INTERFACE.lines() {
            let Some(rest) = line.strip_prefix("  dibs ") else {
                continue;
            };
            let named = word(rest.split_whitespace().next().unwrap());
            let commands = match named.as_str() {
                "<verb>" => vec!["build".to_string(), "test".into(), "bench".into()],
                _ => vec![named],
            };
            for command in commands {
                assert!(short.contains(&command), "{command}");
            }
        }
    }

    #[test]
    fn the_short_help_fits_an_80_column_terminal_of_50_rows() {
        for line in Help::SHORT.lines() {
            assert!(line.chars().count() <= 80, "{line}");
        }
        assert!(Help::short().lines().count() <= 50);
    }

    #[test]
    fn every_entry_a_subcommand_names_is_in_the_whole_help() {
        for topic in Help::TOPICS {
            for begins in topic.entries {
                assert!(Help::entry(begins).is_some(), "{begins}");
            }
        }
    }

    #[test]
    fn a_subcommands_help_is_its_own_entries_and_no_others() {
        let machines = Help::of(Some("machines"));
        assert!(machines.starts_with("  dibs machines [<machine>] [--json]\n"));
        assert!(machines.contains("is probed under its shared lock"));
        assert!(!machines.contains("dibs batch"), "{machines}");

        let build = Help::of(Some("build"));
        assert!(build.contains("  --sweep ") && build.contains("  <ref> "));
        assert!(!build.contains("--friction"), "{build}");
    }

    #[test]
    fn a_word_with_no_help_of_its_own_gets_the_whole_help() {
        assert_eq!(Help::of(Some("nothing")), Help::text());
        assert_eq!(Help::of(Some("all")), Help::text());
        assert_eq!(Help::of(None), Help::short());
    }
}
