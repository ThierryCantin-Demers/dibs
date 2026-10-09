use super::{
    Shell, Wanted,
    flags::{CALL, RECIPE, SUBCOMMANDS},
    sources::Candidate,
};
use crate::cli::RecipeVerb;
use regex::Regex;

fn at(typed: &[&str]) -> Wanted {
    Wanted::at(&typed.iter().map(|w| (*w).to_string()).collect::<Vec<_>>())
}

/// The flags a parser's match arms accept, so a new one cannot go uncompleted.
fn matched(source: &str) -> Vec<String> {
    let arm = Regex::new(r#"^\s*((?:"-[-A-Za-z]*"\s*\|\s*)*"-[-A-Za-z]*")\s*=>"#).unwrap();
    let flag = Regex::new(r#""(-[-A-Za-z]+)""#).unwrap();
    source
        .lines()
        .filter_map(|line| arm.captures(line))
        .flat_map(|c| {
            flag.captures_iter(c.get(1).unwrap().as_str())
                .map(|f| f[1].to_string())
                .collect::<Vec<_>>()
        })
        .filter(|f| f != "--")
        .collect()
}

#[test]
fn every_flag_a_parser_accepts_is_offered() {
    let offered =
        |flags: &[super::flags::Flag], f: &str| flags.iter().any(|g| g.names.contains(&f));
    for f in matched(include_str!("../cli/parse.rs")) {
        assert!(
            offered(CALL, &f),
            "{f} is accepted before a verb and not offered"
        );
    }
    for f in matched(include_str!("../cli/recipe.rs")) {
        assert!(
            offered(RECIPE, &f),
            "{f} is accepted after a verb and not offered"
        );
    }
}

#[test]
fn every_recipe_verb_is_offered_as_a_subcommand() {
    for verb in RecipeVerb::ALL {
        assert!(
            SUBCOMMANDS.iter().any(|s| s.name == verb.as_str()),
            "{verb} is not offered"
        );
    }
}

#[test]
fn what_is_offered_follows_the_words_before_the_cursor() {
    let repo = |r: &str| r.to_string();
    for (typed, wanted) in [
        (vec![""], Wanted::Start),
        (vec!["--"], Wanted::CallFlags),
        (vec!["--on", ""], Wanted::Machines),
        (
            vec!["--on", "m", "--device", ""],
            Wanted::Cards {
                machine: Some("m".into()),
            },
        ),
        (vec!["--device", ""], Wanted::Cards { machine: None }),
        (vec!["--measure", ""], Wanted::Machines),
        (vec!["--measure", "m", ""], Wanted::OnOff),
        (vec!["--measure", "m", "off", "--on", ""], Wanted::Machines),
        (vec!["--label", ""], Wanted::Labels),
        (vec!["--max", ""], Wanted::Nothing),
        (vec!["--bench", "--on", "m", ""], Wanted::Start),
        (vec!["cargo", ""], Wanted::Nothing),
        (vec!["--sync", "-a", ""], Wanted::Paths),
        (vec!["completions", ""], Wanted::Shells),
        (vec!["build", ""], Wanted::Repos),
        (vec!["build", "app@ma"], Wanted::Refs { repo: repo("app") }),
        (
            vec!["build", "app@main", ""],
            Wanted::Recipes {
                verb: RecipeVerb::Build,
                repo: repo("app"),
            },
        ),
        (
            vec!["--on", "m", "bench", "app@local", "p", "--"],
            Wanted::RecipeFlags {
                verb: RecipeVerb::Bench,
                repo: repo("app"),
                recipe: Some("p".into()),
            },
        ),
        (
            vec!["bench", "app@local", "p", "--backend", ""],
            Wanted::Choices {
                verb: RecipeVerb::Bench,
                repo: repo("app"),
                recipe: "p".into(),
                param: "backend".into(),
            },
        ),
        (
            vec!["bench", "app@local", "p", "--backend", "cuda", ""],
            Wanted::Nothing,
        ),
        (
            vec!["bench", "app@local", "p", "--on", ""],
            Wanted::Machines,
        ),
        (
            vec!["test", "--pin", "lib@"],
            Wanted::Refs { repo: repo("lib") },
        ),
        (
            vec!["with", "app", ""],
            Wanted::Services { repo: repo("app") },
        ),
        (vec!["shell", "app@local", "--", ""], Wanted::Nothing),
        (vec!["runs", ""], Wanted::Labels),
        (vec!["batch", ""], Wanted::Paths),
        (vec!["machines", ""], Wanted::Machines),
    ] {
        assert_eq!(at(&typed), wanted, "after {typed:?}");
    }
}

#[test]
fn zsh_takes_a_colon_in_a_value_escaped() {
    let card = Candidate::new("gpu:0", "a card");
    assert_eq!(
        [Shell::Fish, Shell::Zsh].map(|s| s.line(&card)),
        [
            "gpu:0\ta card\n".to_string(),
            "gpu\\:0:a card\n".to_string()
        ]
    );
}
