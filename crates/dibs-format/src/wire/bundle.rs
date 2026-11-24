use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An executable built on one machine and run on another, kept under the scratch's `bundles`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bundle {
    /// After a green build, the executable cargo's own report names, kept with its manifest.
    Make(BundleMake),
    /// Before the command, the bundle there must still be this one.
    Check(BundleCheck),
}

/// What a build keeps, and from which of cargo's artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleMake {
    /// The `cargo build` arguments the step's command built with, as the manifest records them.
    /// The command writes cargo's JSON report of that build to `$DIBS_CARGO_REPORT`.
    pub cargo: Vec<String>,
    pub package: String,
    /// Cargo's target kind: `bin` or `example`.
    pub kind: String,
    pub name: String,
    /// The directory under `bundles` it is kept in.
    pub into: String,
    /// Set for the step's command, `RUSTFLAGS` above all.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// What the caller binds the bundle to besides what the machine reads, recorded as given.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
}

impl BundleMake {
    /// Where the step's command writes cargo's `--message-format=json` report of its build.
    pub const REPORT: &'static str = "DIBS_CARGO_REPORT";
}

/// The bundle a command runs, by the directory under `bundles` and the executable's digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleCheck {
    pub name: String,
    pub digest: String,
}

/// What a bundle's `manifest.json` holds beside its executable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    /// The executable's file name in the bundle.
    pub exe: String,
    /// The SHA-256 of the executable, in hex: the bundle's identity.
    pub digest: String,
    pub size: u64,
    pub package: String,
    pub kind: String,
    pub name: String,
    /// Cargo's profile, as its report names it.
    pub profile: String,
    pub cargo: Vec<String>,
    /// `RUSTFLAGS` as the build saw it.
    pub rustflags: String,
    /// `rustc -vV`.
    pub rustc: String,
    /// The SHA-256 of the tree's `Cargo.lock`, empty when it has none.
    pub lockfile: String,
    /// The tree the build ran in, as the runner names it.
    pub tree: String,
    /// The job that built it.
    pub job: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
}

impl BundleManifest {
    pub const FILE: &'static str = "manifest.json";
    /// Under the scratch.
    pub const DIR: &'static str = "bundles";
}
