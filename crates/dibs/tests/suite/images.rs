use crate::harness::*;
use crate::recipes::{app, recipes};
use std::fs;

const IMAGE: &str = "registry.example/floor@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// skopeo and umoci that leave an empty root filesystem, and a bwrap that records how it was
/// called and runs the command without a sandbox, so the suite needs no registry or namespaces.
fn fakes(s: &Sandbox) {
    let bin = s.path("bin");
    let fake = |name: &str, body: &str| {
        let script = format!(
            "#!/bin/bash\necho \"$*\" >> {}/{name}.calls\n{body}\n",
            s.p("tmp")
        );
        write_exec(&bin.join(name), &script);
    };
    fake("skopeo", r#"d=${4#oci:}; mkdir -p "${d%:image}""#);
    fake(
        "umoci",
        r#"mkdir -p "$5/rootfs/etc" && echo 'root:x:0:0::/root:/bin/bash' > "$5/rootfs/etc/passwd""#,
    );
    fake(
        "bwrap",
        r#"while [ "$1" != -- ]; do shift; done; shift; exec "$@""#,
    );
}

fn calls(s: &Sandbox, tool: &str) -> Vec<String> {
    fs::read_to_string(s.path("tmp").join(format!("{tool}.calls")))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn shell(s: &Sandbox, app: &str, image: &str, command: &str) -> Output {
    let at = format!("{app}@local");
    s.dibs(["shell", &at, "--reason", "test", "--", command])
        .env("DIBS_IMAGE", image)
        .run()
}

fn uid() -> String {
    let id = std::process::Command::new("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&id.stdout).trim().to_string()
}

#[test]
fn a_shared_job_runs_in_its_image_unpacked_once() {
    let s = Sandbox::new();
    fakes(&s);
    let app = app(&s);
    for _ in 0..2 {
        let out = shell(&s, &app, IMAGE, "echo inside");
        assert_eq!(out.code, 0, "{}", out.all());
        assert_eq!(out.stdout.lines_with("inside"), 1, "{}", out.all());
    }
    assert_eq!(calls(&s, "skopeo").len(), 1, "fetched once per machine");
    let rootfs = s.path(
        "scratch/images/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/rootfs",
    );
    let bwrap = calls(&s, "bwrap");
    assert_eq!(bwrap.len(), 2);
    assert!(
        bwrap[0].starts_with(&format!("--ro-bind {} / ", rootfs.display())),
        "{}",
        bwrap[0]
    );
    // Images are Linux's alone, and macOS keeps its accounts out of /etc/passwd.
    if cfg!(target_os = "linux") {
        let passwd = fs::read_to_string(rootfs.join("etc/passwd")).unwrap();
        let ours = passwd
            .lines()
            .filter(|l| l.split(':').nth(2) == Some(uid().as_str()))
            .count();
        assert_eq!(ours, 1, "the account is known inside: {passwd}");
    }
    let targets: Vec<String> = fs::read_dir(s.path("scratch/target"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        targets.iter().any(|t| t.ends_with("-img0123456789ab")),
        "{targets:?}"
    );
}

#[test]
fn a_command_with_no_tree_is_not_built_in_an_image() {
    let s = Sandbox::new();
    fakes(&s);
    let out = s
        .dibs(["--label", "imaged", "echo inside"])
        .env("DIBS_IMAGE", IMAGE)
        .run();
    assert_eq!(out.code, 3, "{}", out.all());
    assert_eq!(out.stderr.lines_with("needs a tree"), 1, "{}", out.all());
    assert!(calls(&s, "skopeo").is_empty());
}

#[test]
fn a_copy_on_this_machine_is_never_built_in_an_image() {
    let s = Sandbox::new();
    fakes(&s);
    s.write("syncsrc/f.txt", "carried\n");
    let (src, dst) = (
        format!("{}/", s.p("syncsrc")),
        format!(":{}/", s.p("syncdst")),
    );
    let out = s
        .dibs(["--sync", "-rlpgo", &src, &dst])
        .env("DIBS_IMAGE", IMAGE)
        .run();
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(s.read("syncdst/f.txt"), "carried\n");
    assert!(calls(&s, "skopeo").is_empty());
}

#[test]
fn a_measurement_runs_natively_whatever_image_its_build_used() {
    let s = Sandbox::new();
    fakes(&s);
    let out = s
        .dibs(["--bench", "--label", "imaged", "echo measured"])
        .env("DIBS_IMAGE", IMAGE)
        .run();
    assert_eq!(out.code, 0, "{}", out.all());
    assert!(calls(&s, "bwrap").is_empty());
    assert!(calls(&s, "skopeo").is_empty());
    let measured = s
        .dibs(["--bench", "--label", "imaged", "echo measured"])
        .env("DIBS_IMAGE", "registry.example/floor:latest")
        .run();
    assert_eq!(measured.code, 3, "{}", measured.all());
}

#[test]
fn an_image_named_without_its_digest_runs_nothing() {
    let s = Sandbox::new();
    fakes(&s);
    let app = app(&s);
    let out = shell(&s, &app, "registry.example/floor:latest", "echo inside");
    assert_eq!(out.code, 3, "{}", out.all());
    assert_eq!(
        out.stderr.lines_with("only a digest pins"),
        1,
        "{}",
        out.all()
    );
    assert!(calls(&s, "skopeo").is_empty());
}

#[test]
#[ignore = "fetches a real image and needs skopeo, umoci and bwrap"]
fn a_real_image_gives_its_own_glibc_and_cannot_be_changed() {
    let s = Sandbox::new();
    let app = app(&s);
    let image = "docker.io/library/buildpack-deps:noble@sha256:924bf5a2e44d8ad64669df739cc3a7d331dfb0a5b41815f0ca71a4819a503e8a";
    let out = shell(
        &s,
        &app,
        image,
        "ldd --version | head -1; id -un; touch $DIBS_SCRATCH/images/x || echo sealed",
    );
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(out.stdout.lines_with("2.39"), 1, "{}", out.all());
    assert_eq!(out.stdout.lines_with("sealed"), 1, "{}", out.all());
    assert_eq!(out.stdout.lines_with("I have no name"), 0, "{}", out.all());
}

#[test]
fn gc_removes_an_image_nobody_used_lately_unless_a_job_runs_in_it() {
    let s = Sandbox::new();
    let image = |digest: char| format!("scratch/images/{}", digest.to_string().repeat(64));
    for digest in ['a', 'b', 'c'] {
        s.write(&format!("{}/rootfs/usr/lib/libc.so.6", image(digest)), "x");
        s.write(&format!("{}/.dibs-used", image(digest)), "");
    }
    for digest in ['a', 'b'] {
        let used = s.p(&format!("{}/.dibs-used", image(digest)));
        assert_eq!(s.command("touch", ["-d", "30 days ago", &used]).code(), 0);
    }
    let unwritable = s.p(&format!("{}/rootfs/usr/lib", image('a')));
    assert_eq!(s.command("chmod", ["555", &unwritable]).code(), 0);
    let held =
        fs::File::create(s.path(&format!("scratch/images/.{}.use", "b".repeat(64)))).unwrap();
    held.lock_shared().unwrap();
    let out = s.dibs(["--gc"]).run().all();
    assert!(
        !s.exists(&image('a')),
        "an image unused for a month goes: {out}"
    );
    assert!(s.exists(&image('b')), "unless a job runs in it: {out}");
    assert_eq!(out.lines_matching(r"images/b+ .* in use"), 1, "{out}");
    assert!(s.exists(&image('c')), "and one used today stays: {out}");

    s.write("scratch/images/photos/cat.jpg", "x");
    let named = format!("scratch/images/{}/notes", "e".repeat(64));
    s.write(&named, "x");
    let stopped = format!("scratch/images/.gone-{}-{}", u32::MAX, "d".repeat(12));
    s.write(&format!("{stopped}/rootfs/x"), "x");
    let out = s.dibs(["--gc", "--all"]).run().all();
    assert!(
        s.exists("scratch/images/photos/cat.jpg"),
        "what dibs did not make there stays: {out}"
    );
    assert!(s.exists(&named), "even named like an image: {out}");
    assert_eq!(
        out.lines_matching(r"images/photos .* not an image, never removed"),
        1,
        "and is shown: {out}"
    );
    assert!(
        !s.exists(&stopped),
        "a removal stopped partway is finished: {out}"
    );
    assert!(
        s.exists(&image('b')),
        "and one in use stays, whatever its clock: {out}"
    );
}

#[test]
fn the_flag_names_an_image_over_the_recipes_own() {
    let s = Sandbox::new();
    fakes(&s);
    let app = app(&s);
    let other = format!("registry.example/other@sha256:{}", "f".repeat(64));
    recipes(
        &s,
        &format!(
            "[build.b]\nimage = \"{IMAGE}\"\n[[build.b.step]]\nlock = \"shared\"\nrun = \"echo built\"\n"
        ),
    );
    let at = format!("{app}@local");
    let out = s.dibs(["build", &at, "b"]).run();
    assert_eq!(out.code, 0, "{}", out.all());
    let fetched = calls(&s, "skopeo");
    assert_eq!(fetched.len(), 1, "the recipe's image: {fetched:?}");
    assert!(fetched[0].contains("0123456789abcdef"), "{fetched:?}");
    let out = s.dibs(["build", &at, "b", "--image", &other]).run();
    assert_eq!(out.code, 0, "{}", out.all());
    let fetched = calls(&s, "skopeo");
    assert!(
        fetched.last().unwrap().contains("ffffffff"),
        "the flag's: {fetched:?}"
    );
    let out = s
        .dibs([
            "shell",
            &at,
            "--reason",
            "test",
            "--image",
            &other,
            "--",
            "echo inside",
        ])
        .run();
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(calls(&s, "bwrap").len(), 3, "a shell takes the flag too");
}

#[test]
fn a_swept_recipe_runs_in_its_image() {
    let s = Sandbox::new();
    fakes(&s);
    let app = app(&s);
    recipes(
        &s,
        &format!(
            "[build.b]\nimage = \"{IMAGE}\"\n[build.b.params.size]\n[[build.b.step]]\nlock = \"shared\"\nrun = \"echo {{size}}\"\n"
        ),
    );
    let out = s
        .dibs(["build", &format!("{app}@local"), "b", "--sweep", "size=32"])
        .run();
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(calls(&s, "bwrap").len(), 1, "{}", out.all());
    let out = s
        .dibs(["build", &format!("{app}@local"), "b", "--size", "64"])
        .env("DIBS_IMAGE", "")
        .run();
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(
        calls(&s, "bwrap").len(),
        2,
        "an empty DIBS_IMAGE names none: {}",
        out.all()
    );
}

#[test]
fn a_batch_followed_after_a_check_in_keeps_its_image() {
    let mut s = Sandbox::new();
    fakes(&s);
    let app = app(&s);
    let (up, hold) = (s.gate("up"), s.gate("hold"));
    s.write(
        "b",
        &format!(
            "[long] dibs shell {app}@local --reason test -- '{}; {}'\n\
             [next] dibs shell {app}@local --reason test -- 'echo next'\n",
            up.signal(),
            hold.hold()
        ),
    );
    let paused = s
        .dibs(["batch", &s.p("b")])
        .env("DIBS_IMAGE", IMAGE)
        .env("DIBS_CHECK_IN", "1s")
        .run();
    assert_eq!(paused.code, 80, "{}", paused.all());
    let id = capture(&paused.stderr, r"batch ([0-9-]+) checked in")
        .unwrap_or_else(|| panic!("no check-in: {}", paused.all()));
    up.reached();
    let follow = s.spawn(s.dibs(["batch", "--follow", &id]));
    hold.open();
    assert_eq!(s.wait(follow), 0);
    assert_eq!(
        calls(&s, "bwrap").len(),
        2,
        "the step a follow starts runs in it too"
    );
}

#[test]
fn a_recipes_image_without_its_digest_is_refused_before_anything_runs() {
    let s = Sandbox::new();
    fakes(&s);
    let app = app(&s);
    recipes(
        &s,
        "[build.b]\nimage = \"registry.example/floor:latest\"\n[[build.b.step]]\nlock = \"shared\"\nrun = \"echo built\"\n",
    );
    let out = s.dibs(["build", &format!("{app}@local"), "b"]).run();
    assert_ne!(out.code, 0, "{}", out.all());
    assert_eq!(
        out.stderr.lines_with("has to be pinned"),
        1,
        "{}",
        out.all()
    );
    assert!(calls(&s, "skopeo").is_empty());
}
