use super::{
    labels::{label_steps, run_label},
    manifest::{Isolation, Manifest, Recipe, Source, Step, Verb},
};
use dibs_format::Lock;
use std::{collections::BTreeMap, path::Path};

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), body).unwrap();
}

/// Local wins, because it is the override, and because these are shared upstream repos:
/// a recipe still moving cannot live in one without costing a pull request.
#[test]
fn local_config_overrides_the_repo() {
    let tmp = std::env::temp_dir().join(format!("dibs-recipe-{}", std::process::id()));
    let repo = tmp.join("cubek");
    let cfg = tmp.join("cfg");
    write(
        &repo,
        ".dibs.toml",
        "[build.x]\n[[build.x.step]]\nlock=\"shared\"\nrun=\"from repo\"\n",
    );
    write(
        &cfg,
        "cubek.toml",
        "[build.x]\n[[build.x.step]]\nlock=\"shared\"\nrun=\"from local\"\n",
    );
    let m = Manifest::load_from(&repo, "cubek", &cfg).unwrap();
    let r = m.recipe(Verb::Build, "x").unwrap();
    assert_eq!(r.steps[0].run, "from local");
    assert_eq!(
        r.source,
        Source::Local,
        "an override has to be visible as one"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_service_carries_its_servers_ports_and_what_makes_them_ready() {
    let tmp = std::env::temp_dir().join(format!("dibs-service-{}", std::process::id()));
    let repo = tmp.join("app");
    let cfg = tmp.join("cfg");
    write(
        &repo,
        ".dibs.toml",
        "[service.gpus]\nbuild=\"cargo build -p server\"\nports=[\"cuda\",\"vulkan\"]\n\
         [[service.gpus.serve]]\nname=\"cuda\"\nrun=\"server --listen :$DIBS_PORT_CUDA\"\nready=\"tcp:cuda\"\n\
         [[service.gpus.serve]]\nname=\"vulkan\"\nrun=\"server --vulkan\"\n",
    );
    let m = Manifest::load_from(&repo, "app", &cfg).unwrap();
    let svc = m.service("gpus").unwrap();
    assert_eq!(svc.build.as_deref(), Some("cargo build -p server"));
    assert_eq!(svc.ports, ["cuda", "vulkan"]);
    assert_eq!(svc.serves.len(), 2);
    assert_eq!(
        (svc.serves[0].name.as_str(), svc.serves[0].ready.as_deref()),
        ("cuda", Some("tcp:cuda"))
    );
    assert_eq!(
        svc.serves[1].ready, None,
        "a server may say nothing about being ready"
    );
    assert_eq!(svc.source, Source::Repo);
    let _ = std::fs::remove_dir_all(&tmp);
}

fn parse(body: &str) -> Recipe {
    let m: Manifest = toml::from_str(body).unwrap();
    m.bench.into_iter().next().unwrap().1
}

#[test]
fn a_fresh_variable_changes_the_procedure_and_has_to_be_a_variable() {
    let plain = parse("[bench.r]\n[[bench.r.step]]\nlock = \"shared\"\nrun = \"x\"\n");
    let fresh = parse(
        "[bench.r]\nfresh = [\"CUBECL_ENVIRONMENT\"]\n[[bench.r.step]]\nlock = \"shared\"\nrun = \"x\"\n",
    );
    assert_eq!(fresh.fresh, ["CUBECL_ENVIRONMENT"]);
    assert_ne!(
        plain.fingerprint(),
        fresh.fingerprint(),
        "a cold cache and a warm one are two procedures"
    );
    let bad =
        parse("[bench.r]\nfresh = [\"A B\"]\n[[bench.r.step]]\nlock = \"shared\"\nrun = \"x\"\n");
    assert!(
        bad.check("r")
            .unwrap_err()
            .contains("'A B' is not a variable name")
    );
}

const SWEEP: &str = "\
[bench.r.params]\n\
backend = { choices = [\"cuda\", \"vulkan\"], default = \"cuda\" }\n\
samples = { default = \"10\" }\n\
size = {}\n\
[[bench.r.step]]\n\
lock = \"shared\"\n\
run = \"cargo build --features cubecl/{backend}\"\n\
[[bench.r.step]]\n\
lock = \"exclusive\"\n\
env = { SAMPLES = \"{samples}\", SHAPE = \"{size}x{size}\" }\n\
run = \"cargo bench --features cubecl/{backend} -- $FILTER\"\n";

#[test]
fn a_parameter_falls_back_to_its_default_and_is_checked_against_its_choices() {
    let r = parse(SWEEP);
    let given = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let v = r.values(&given(&[("size", "64")])).unwrap();
    assert_eq!(v["backend"], "cuda");
    assert_eq!(v["samples"], "10");

    let e = r
        .values(&given(&[("backend", "metal"), ("size", "64")]))
        .unwrap_err();
    assert!(
        e.contains("cuda, vulkan"),
        "a refusal has to say what is allowed: {e}"
    );
    let e = r
        .values(&given(&[("backends", "cuda"), ("size", "64")]))
        .unwrap_err();
    assert!(
        e.contains("backend, samples, size"),
        "and which names exist: {e}"
    );
    let e = r.values(&given(&[])).unwrap_err();
    assert!(
        e.contains("--size"),
        "a parameter with no default cannot be left out: {e}"
    );
}

#[test]
fn binding_fills_the_declared_names_and_leaves_the_shell_alone() {
    let r = parse(SWEEP);
    let given: BTreeMap<String, String> = [("backend", "vulkan"), ("size", "64")]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let b = r.bound(&r.values(&given).unwrap());
    assert_eq!(b.steps[0].run, "cargo build --features cubecl/vulkan");
    assert_eq!(b.steps[1].env["SAMPLES"], "10");
    assert_eq!(
        b.steps[1].env["SHAPE"], "64x64",
        "a name can appear twice in one value"
    );
    assert!(
        b.steps[1].run.ends_with("-- $FILTER"),
        "the command is shell, not a template"
    );
    assert_ne!(
        r.fingerprint(),
        b.fingerprint(),
        "what ran is what has to be identified"
    );
}

#[test]
fn a_step_cannot_name_the_target_directory_it_does_not_write_to() {
    let r = parse("[[bench.r.step]]\nlock=\"shared\"\nrun=\"ls target/release/bench\"\n");
    let e = r.check("r").unwrap_err();
    assert!(e.contains("CARGO_TARGET_DIR"), "{e}");
    let fine = parse(
        "[[bench.r.step]]\nlock=\"shared\"\nrun=\"ls $CARGO_TARGET_DIR/release && ls $R/target-main\"\n",
    );
    fine.check("r")
        .expect("an absolute target directory is the whole point");
    for run in [
        "cd target",
        "ls target/x86_64-unknown-linux-gnu/release",
        "cat target/criterion/r",
    ] {
        parse(&format!(
            "[[bench.r.step]]\nlock=\"shared\"\nrun=\"{run}\"\n"
        ))
        .check("r")
        .unwrap_err();
    }
    let own = parse(
        "[[bench.r.step]]\nlock=\"shared\"\nrun=\"rm -rf target/guide && ls ./target/guide/model\"\n",
    );
    own.check("r")
        .expect("a program's own directory under target/ is in the tree");
}

#[test]
fn a_measurement_that_would_compile_under_the_exclusive_lock_is_refused() {
    let alone = parse("[[bench.r.step]]\nlock=\"exclusive\"\nrun=\"cargo bench --bench gemm\"\n");
    let e = alone.check("r").unwrap_err();
    assert!(
        e.contains("--no-run"),
        "the message has to show the two-step form: {e}"
    );
    let split = parse(
        "[[bench.r.step]]\nlock=\"shared\"\nrun=\"cargo bench --no-run\"\n\
         [[bench.r.step]]\nlock=\"exclusive\"\nrun=\"cargo bench --bench gemm\"\n",
    );
    split
        .check("r")
        .expect("built shared then measured exclusive is the shape being asked for");
    let prebuilt =
        parse("[[bench.r.step]]\nlock=\"exclusive\"\nrun=\"$CARGO_TARGET_DIR/release/bench\"\n");
    prebuilt
        .check("r")
        .expect("a binary that was already built compiles nothing");
}

#[test]
fn the_last_layer_to_describe_the_tree_decides_what_a_new_one_starts_without() {
    let tmp = std::env::temp_dir().join(format!("dibs-tree-{}", std::process::id()));
    let (repo, cfg) = (tmp.join("app"), tmp.join("cfg"));
    write(&repo, ".dibs.toml", "[tree]\nfresh = [\"cache\"]\n");
    assert_eq!(
        Manifest::load_from(&repo, "app", &cfg)
            .unwrap()
            .tree_fresh(),
        ["cache"]
    );
    write(
        &cfg,
        "app.toml",
        "[tree]\nfresh = [\"target/environment\"]\n",
    );
    assert_eq!(
        Manifest::load_from(&repo, "app", &cfg)
            .unwrap()
            .tree_fresh(),
        ["target/environment"]
    );
    write(
        &cfg,
        "app.toml",
        "[build.x]\n[[build.x.step]]\nlock=\"shared\"\nrun=\"make\"\n",
    );
    assert_eq!(
        Manifest::load_from(&repo, "app", &cfg)
            .unwrap()
            .tree_fresh(),
        ["cache"],
        "a layer that says nothing about it changes nothing"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_tree_path_that_could_reach_outside_the_tree_is_refused() {
    let tmp = std::env::temp_dir().join(format!("dibs-tree-bad-{}", std::process::id()));
    for bad in [
        "..", "a/../..", "/etc", ".", "", "a//b", "a/", "a b", "$HOME", "*",
    ] {
        write(&tmp, ".dibs.toml", &format!("[tree]\nfresh = [{bad:?}]\n"));
        let e = Manifest::load_from(&tmp, "app", &tmp.join("cfg"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("[tree] fresh"), "{bad:?}: {e}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_repo_with_no_recipes_anywhere_says_where_it_looked() {
    let tmp = std::env::temp_dir().join(format!("dibs-none-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let e = Manifest::load_from(&tmp, "nothing", &tmp.join("empty"))
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("nothing in"),
        "an error has to say where it looked: {e}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

fn step(lock: Lock, run: &str) -> Step {
    Step {
        lock,
        run: run.into(),
        env: BTreeMap::new(),
    }
}

#[test]
fn the_fingerprint_follows_the_procedure_and_nothing_else() {
    let a = Recipe {
        source: Source::Repo,
        needs: None,
        isolation: Isolation::Machine,
        params: BTreeMap::new(),
        fresh: Vec::new(),
        artifacts: Vec::new(),
        steps: vec![step(Lock::Shared, "cargo build")],
    };
    let same = Recipe {
        source: Source::Repo,
        needs: None,
        isolation: Isolation::Machine,
        params: BTreeMap::new(),
        fresh: Vec::new(),
        artifacts: Vec::new(),
        steps: vec![step(Lock::Shared, "cargo build")],
    };
    let changed_command = Recipe {
        source: Source::Repo,
        needs: None,
        isolation: Isolation::Machine,
        params: BTreeMap::new(),
        fresh: Vec::new(),
        artifacts: Vec::new(),
        steps: vec![step(Lock::Shared, "cargo build --release")],
    };
    let changed_lock = Recipe {
        source: Source::Repo,
        needs: None,
        isolation: Isolation::Machine,
        params: BTreeMap::new(),
        fresh: Vec::new(),
        artifacts: Vec::new(),
        steps: vec![step(Lock::Exclusive, "cargo build")],
    };
    assert_eq!(a.fingerprint(), same.fingerprint());
    assert_ne!(a.fingerprint(), changed_command.fingerprint());
    // A step moved from the shared lock to the exclusive one is a different procedure
    // even though it runs the same command, and comparing across it would be wrong.
    assert_ne!(a.fingerprint(), changed_lock.fingerprint());
}

#[test]
fn isolation_defaults_to_the_whole_machine() {
    let r: Recipe = toml::from_str("[[step]]\nlock = \"shared\"\nrun = \"x\"").unwrap();
    assert_eq!(r.isolation, Isolation::Machine);
}

#[test]
fn a_build_and_a_measurement_need_no_suffix() {
    let steps = vec![step(Lock::Shared, "build"), step(Lock::Exclusive, "bench")];
    assert_eq!(label_steps("r/x", &steps), vec!["r/x", "r/x"]);
}

#[test]
fn a_recipe_name_is_only_unique_within_its_verb() {
    assert_ne!(
        run_label("cubek", "build", Some("cuda"), None),
        run_label("cubek", "test", Some("cuda"), None)
    );
}

#[test]
fn two_steps_taking_the_same_lock_must_not_share_a_label() {
    let steps = vec![
        step(Lock::Shared, "one"),
        step(Lock::Shared, "two"),
        step(Lock::Exclusive, "measure"),
    ];
    assert_eq!(label_steps("r/x", &steps), vec!["r/x.1", "r/x.2", "r/x"]);
}

#[test]
fn a_card_gets_its_own_label_and_an_unpinned_run_is_left_alone() {
    let pinned = run_label("cubecl", "bench", Some("throughput-all"), Some("gpu:a"));
    let other = run_label("cubecl", "bench", Some("throughput-all"), Some("gpu:b"));
    assert_ne!(pinned, other);
    assert_eq!(
        run_label("cubecl", "bench", Some("throughput-all"), None),
        "cubecl/bench/throughput-all"
    );
}
