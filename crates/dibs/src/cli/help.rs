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
                "--on <machine>",
                "--wait <seconds>",
                "--max <seconds>",
                "--label <name>",
            ],
        },
        Topic {
            words: &["status"],
            entries: &[
                "dibs --status",
                "dibs --watch",
                "--json",
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
            entries: &["dibs hook ssh"],
        },
    ];

    /// `dibs --help`: the interface, then what each exit means.
    pub fn text() -> String {
        let mut text = Help::INTERFACE.to_string();
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

    /// What `--help` after a subcommand prints: that subcommand's part of the whole help, or
    /// the whole help for a word that has none.
    pub fn of(subcommand: Option<&str>) -> String {
        let topic =
            subcommand.and_then(|word| Help::TOPICS.iter().find(|t| t.words.contains(&word)));
        let Some(topic) = topic else {
            return Help::text();
        };
        let mut text: String = topic
            .entries
            .iter()
            .filter_map(|e| Help::entry(e))
            .collect();
        text.push_str("\ndibs --help shows every subcommand and flag, and what each exit means.\n");
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
        assert_eq!(Help::of(None), Help::text());
    }
}
