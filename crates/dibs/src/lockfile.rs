//! A Cargo.lock, read here.

/// One `[[package]]` of a Cargo.lock. A package without a source is a path in the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: Option<String>,
    pub source: Option<String>,
}

impl Package {
    /// Every package of a lockfile, in its order.
    pub fn all(lock: &str) -> Vec<Package> {
        let mut out = Vec::new();
        let mut current: Option<Package> = None;
        let value = |v: &str| v.trim_matches('"').to_string();
        for line in lock.lines().map(str::trim) {
            if line == "[[package]]" {
                out.extend(current.take());
                current = Some(Package {
                    name: String::new(),
                    version: None,
                    source: None,
                });
            } else if let Some(package) = current.as_mut() {
                if let Some(v) = line.strip_prefix("name = ") {
                    package.name = value(v);
                } else if let Some(v) = line.strip_prefix("version = ") {
                    package.version = Some(value(v));
                } else if let Some(v) = line.strip_prefix("source = ") {
                    package.source = Some(value(v));
                }
            }
        }
        out.extend(current);
        out.retain(|p| !p.name.is_empty());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_package_is_read_with_what_it_declares() {
        let lock = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        let packages = Package::all(lock);
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].source, None);
        assert_eq!(packages[1].version.as_deref(), Some("1.0.0"));
    }
}
