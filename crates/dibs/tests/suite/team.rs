//! The team clone: `dibs --join`, and a recipes clone moved to where the team's is read.

use crate::harness::*;

/// A repo a team shares, with a recipe file at `recipe` and two machines.
fn team_origin(s: &Sandbox, recipe: &str) -> String {
    s.git(".", &["init", "-q", "-b", "main", "origin"]);
    s.write(&format!("origin/{recipe}"), "[build.x]\n");
    s.write(
        "origin/machines.toml",
        "[machine.rig]\nssh = \"dibs@rig.example\"\n\n[machine.mac]\nssh = \"dibs@mac.example\"\n",
    );
    s.git("origin", &["add", "-A"]);
    s.git("origin", &["commit", "-qm", "team"]);
    s.p("origin")
}

fn which(s: &Sandbox, machine: &str) -> String {
    s.dibs(["--on", machine, "--which"]).run().stdout
}

#[test]
fn joining_clones_the_teams_repo_and_says_which_machines_cannot_be_reached_yet() {
    let s = Sandbox::new();
    let origin = team_origin(&s, "recipes/app.toml");
    s.write(
        "home/.config/dibs/machines.toml",
        "[machine.rig]\nssh = \"dibs@rig.home\"\n",
    );
    let out = s.dibs(["--join", &origin]).run();
    assert_eq!(out.code, 0, "{}", out.all());
    for said in [
        "machines: rig, mac",
        "recipes:  app",
        "Your own machines.toml also has rig",
        "rig: cannot be reached yet",
        "mac: cannot be reached yet",
        "'rig.home' does not resolve",
        "'mac.example' does not resolve",
    ] {
        assert_eq!(out.stdout.lines_with(said), 1, "{said}: {}", out.all());
    }
    assert_eq!(which(&s, "mac"), "mac\n", "its machines are read");

    let again = s.dibs(["--join", &origin]).run();
    assert_eq!(
        (
            again.code,
            again.stderr.lines_with("is already a team clone")
        ),
        (2, 1),
        "{}",
        again.all()
    );
}

#[test]
fn a_repo_that_does_not_clone_leaves_no_team_behind() {
    let s = Sandbox::new();
    let out = s.dibs(["--join", &s.p("nowhere")]).run();
    assert_eq!(
        (out.code, out.stderr.lines_with("could not clone")),
        (1, 1),
        "{}",
        out.all()
    );
    assert!(!s.path("home/.config/dibs/team").exists());
}

#[test]
fn a_recipes_clone_moves_to_where_the_teams_is_read_once_it_has_the_teams_layout() {
    let s = Sandbox::new();
    let origin = team_origin(&s, "app.toml");
    s.git(".", &["clone", "-q", &origin, "home/.config/dibs/recipes"]);
    let out = s.dibs(["--on", "mac", "--which"]).run();
    assert_eq!(
        (out.code, out.stderr.lines_with("moved")),
        (2, 0),
        "a clone of recipes alone stays: {}",
        out.all()
    );

    s.git("origin", &["mv", "app.toml", "recipes.toml"]);
    s.write("origin/recipes/app.toml", "[build.x]\n");
    s.git("origin", &["add", "-A"]);
    s.git("origin", &["commit", "-qm", "the team's layout"]);
    s.git("home/.config/dibs/recipes", &["pull", "-q", "--ff-only"]);
    let out = s.dibs(["--on", "mac", "--which"]).run();
    assert_eq!(
        (out.stdout.as_str(), out.stderr.lines_with("so it moved to")),
        ("mac\n", 1),
        "{}",
        out.all()
    );
    assert!(s.path("home/.config/dibs/team/recipes/app.toml").is_file());
    assert!(!s.path("home/.config/dibs/recipes").exists());
}
