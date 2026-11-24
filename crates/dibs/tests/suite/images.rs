use crate::harness::*;
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

#[test]
fn a_shared_job_runs_in_its_image_unpacked_once() {
    let s = Sandbox::new();
    fakes(&s);
    for _ in 0..2 {
        let out = s
            .dibs(["--label", "imaged", "echo inside"])
            .env("DIBS_IMAGE", IMAGE)
            .run();
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
    assert_eq!(
        passwd.lines().count(),
        2,
        "the account is known inside: {passwd}"
    );
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
    let out = s
        .dibs(["--label", "imaged", "echo inside"])
        .env("DIBS_IMAGE", "registry.example/floor:latest")
        .run();
    assert_eq!(out.code, 3, "{}", out.all());
    assert_eq!(
        out.stderr.lines_with("only a digest pins"),
        1,
        "{}",
        out.all()
    );
    assert!(calls(&s, "skopeo").is_empty());
}
