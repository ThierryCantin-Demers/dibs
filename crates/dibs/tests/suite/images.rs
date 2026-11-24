use crate::harness::*;
use crate::recipes::app;
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
    let passwd = fs::read_to_string(rootfs.join("etc/passwd")).unwrap();
    let ours = passwd
        .lines()
        .filter(|l| l.split(':').nth(2) == Some(uid().as_str()))
        .count();
    assert_eq!(ours, 1, "the account is known inside: {passwd}");
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
