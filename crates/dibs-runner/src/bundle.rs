//! Executables built here to run elsewhere, or built elsewhere to run here, under the scratch's
//! `bundles`: kept from exactly what cargo reports it made, and checked by digest before a
//! command runs one.

use crate::job::Environment;
use dibs_format::{
    JobId, Sha256,
    wire::{BundleCheck, BundleMake, BundleManifest},
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub struct Bundles {
    dir: PathBuf,
}

impl Bundles {
    pub fn under(scratch: &Path) -> Bundles {
        Bundles {
            dir: scratch.join(BundleManifest::DIR),
        }
    }

    fn named(&self, name: &str) -> Result<PathBuf, String> {
        let one_component = !name.is_empty()
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        match one_component {
            true => Ok(self.dir.join(name)),
            false => Err(format!("{name:?} names no bundle: one path component")),
        }
    }

    /// The bundle's directory, when it is still the one the caller means.
    pub fn check(&self, check: &BundleCheck) -> Result<PathBuf, String> {
        let dir = self.named(&check.name)?;
        let manifest: BundleManifest = fs::read(dir.join(BundleManifest::FILE))
            .map_err(|e| format!("bundle {} has no manifest here: {e}", check.name))
            .and_then(|text| {
                serde_json::from_slice(&text)
                    .map_err(|e| format!("bundle {}'s manifest is unreadable: {e}", check.name))
            })?;
        if manifest.digest != check.digest {
            return Err(format!(
                "bundle {} here is {}, not {}",
                check.name, manifest.digest, check.digest
            ));
        }
        let exe = self.named(&manifest.exe).map(|_| dir.join(&manifest.exe))?;
        let digest = Sha256::hex_of_file(&exe).map_err(|e| {
            format!(
                "bundle {}'s {} cannot be read: {e}",
                check.name, manifest.exe
            )
        })?;
        match digest == check.digest {
            true => Ok(dir),
            false => Err(format!(
                "bundle {}'s {} was displaced: its digest is {digest}, not {}",
                check.name, manifest.exe, check.digest
            )),
        }
    }

    /// Asks cargo again, in the job's tree and environment, what the build made, and keeps the
    /// one executable its report names for the package and target.
    pub fn make(
        &self,
        make: &BundleMake,
        environment: &Environment,
        job: &JobId,
        host: &str,
    ) -> Result<BundleManifest, String> {
        let into = self.named(&make.into)?;
        let mut cargo = Command::new("cargo");
        cargo
            .args(&make.cargo)
            .arg("--message-format=json-render-diagnostics")
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        environment.apply(&mut cargo);
        let out = cargo
            .output()
            .map_err(|e| format!("cargo could not be asked what it built: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "cargo's report of the build failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let exe = Report(&String::from_utf8_lossy(&out.stdout)).executable(make)?;
        let tree = environment.dir().map(Path::to_path_buf).unwrap_or_default();
        let mut rustc = Command::new("rustc");
        rustc.arg("-vV").stdin(Stdio::null()).stderr(Stdio::null());
        environment.apply(&mut rustc);
        let rustc = rustc
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let lockfile = Sha256::hex_of_file(&tree.join("Cargo.lock")).unwrap_or_default();
        let file = exe
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("cargo named {} as the executable", exe.display()))?;
        let part = self
            .dir
            .join(format!(".{}.{}", make.into, std::process::id()));
        let _ = fs::remove_dir_all(&part);
        fs::create_dir_all(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        fs::copy(&exe, part.join(&file))
            .map_err(|e| format!("{} could not be kept: {e}", exe.display()))?;
        let digest = Sha256::hex_of_file(&part.join(&file)).map_err(|e| format!("{file}: {e}"))?;
        let manifest = BundleManifest {
            size: fs::metadata(part.join(&file)).map(|m| m.len()).unwrap_or(0),
            exe: file,
            digest,
            package: make.package.clone(),
            kind: make.kind.clone(),
            name: make.name.clone(),
            profile: profile(&make.cargo),
            cargo: make.cargo.clone(),
            rustflags: make.env.get("RUSTFLAGS").cloned().unwrap_or_default(),
            rustc,
            lockfile,
            tree: tree.display().to_string(),
            job: job.to_string(),
            host: host.to_string(),
            inputs: make.inputs.clone(),
        };
        let text = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        fs::write(part.join(BundleManifest::FILE), text)
            .map_err(|e| format!("the manifest could not be written: {e}"))?;
        let _ = fs::remove_dir_all(&into);
        fs::rename(&part, &into).map_err(|e| format!("{}: {e}", into.display()))?;
        Ok(manifest)
    }
}

/// Cargo's profile, as its arguments name it.
fn profile(args: &[String]) -> String {
    let mut words = args.iter();
    let mut named = None;
    while let Some(word) = words.next() {
        match word.as_str() {
            "--release" | "-r" => named = named.or(Some("release".to_string())),
            "--profile" => named = words.next().cloned(),
            other => {
                if let Some(given) = other.strip_prefix("--profile=") {
                    named = Some(given.to_string());
                }
            }
        }
    }
    named.unwrap_or_else(|| "dev".to_string())
}

/// `cargo build --message-format=json`'s lines.
struct Report<'a>(&'a str);

impl Report<'_> {
    fn executable(&self, make: &BundleMake) -> Result<PathBuf, String> {
        let found: Vec<PathBuf> = self
            .0
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|v| v["reason"] == "compiler-artifact")
            .filter(|v| v["target"]["name"] == make.name.as_str())
            .filter(|v| {
                v["target"]["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|k| k == make.kind.as_str()))
            })
            .filter(|v| {
                v["package_id"]
                    .as_str()
                    .is_some_and(|id| package_name(id) == make.package)
            })
            .filter_map(|v| v["executable"].as_str().map(PathBuf::from))
            .collect();
        match found.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(format!(
                "cargo's report names no executable for {} {} of package {}",
                make.kind, make.name, make.package
            )),
            many => Err(format!(
                "cargo's report names {} executables for {} {} of package {}, so none is taken",
                many.len(),
                make.kind,
                make.name,
                make.package
            )),
        }
    }
}

/// The package a cargo package id names, in either of the shapes cargo has written: `name
/// version (source)`, or a spec `source#name@version`, `source#version` when the source's last
/// path segment is the name.
fn package_name(id: &str) -> String {
    if let Some((source, fragment)) = id.split_once('#') {
        return match fragment.split_once('@') {
            Some((name, _)) => name.to_string(),
            None => source
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string(),
        };
    }
    id.split(' ').next().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make() -> BundleMake {
        BundleMake {
            cargo: vec!["build".into(), "--release".into()],
            package: "mnist".into(),
            kind: "example".into(),
            name: "mnist".into(),
            into: "b".into(),
            env: Default::default(),
            inputs: Default::default(),
        }
    }

    #[test]
    fn a_package_is_named_in_every_shape_cargo_writes_its_id() {
        assert_eq!(
            package_name("path+file:///w/burn/examples/mnist#0.21.0"),
            "mnist"
        );
        assert_eq!(
            package_name("path+file:///w/burn/examples/mnist-ex#mnist@0.21.0"),
            "mnist"
        );
        assert_eq!(
            package_name("mnist 0.21.0 (path+file:///w/burn/examples/mnist)"),
            "mnist"
        );
    }

    #[test]
    fn the_executable_is_the_one_cargo_names_for_that_package_and_target() {
        let report = [
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/a#0.1.0","target":{"kind":["example"],"name":"mnist"},"executable":"/t/release/examples/a-mnist"}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/mnist#0.1.0","target":{"kind":["lib"],"name":"mnist"},"executable":null}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/mnist#0.1.0","target":{"kind":["example"],"name":"mnist"},"executable":"/t/release/examples/mnist"}"#,
            r#"{"reason":"build-finished","success":true}"#,
        ]
        .join("\n");
        assert_eq!(
            Report(&report).executable(&make()).unwrap(),
            PathBuf::from("/t/release/examples/mnist")
        );
        assert!(Report("").executable(&make()).is_err());
    }

    #[test]
    fn a_profile_is_read_from_cargo_s_arguments() {
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(profile(&words("build --release -p x")), "release");
        assert_eq!(profile(&words("build --profile bench")), "bench");
        assert_eq!(profile(&words("build --profile=ci")), "ci");
        assert_eq!(profile(&words("build -p x")), "dev");
    }

    #[test]
    fn a_bundle_is_checked_by_its_digest() {
        let scratch = std::env::temp_dir().join(format!("dibs-bundles-{}", std::process::id()));
        let _ = fs::remove_dir_all(&scratch);
        let dir = scratch.join("bundles/b");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mnist"), "the program").unwrap();
        let digest = Sha256::hex(b"the program");
        let manifest = BundleManifest {
            exe: "mnist".into(),
            digest: digest.clone(),
            ..BundleManifest::default()
        };
        fs::write(
            dir.join(BundleManifest::FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let bundles = Bundles::under(&scratch);
        let check = |digest: &str| {
            bundles.check(&BundleCheck {
                name: "b".into(),
                digest: digest.into(),
            })
        };
        assert_eq!(check(&digest).unwrap(), dir);
        assert!(check("00").is_err(), "another digest is not this bundle");
        fs::write(dir.join("mnist"), "another program").unwrap();
        assert!(check(&digest).unwrap_err().contains("displaced"));
        assert!(
            bundles
                .check(&BundleCheck {
                    name: "../b".into(),
                    digest
                })
                .is_err()
        );
        let _ = fs::remove_dir_all(&scratch);
    }
}
